// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use failure::format_err;
use fidl::encoding2::OutOfLine;
use fidl::endpoints2::RequestStream;
use fidl_fuchsia_wlan_device_service::{self as fidl_svc, DeviceServiceRequest};
use fidl_fuchsia_wlan_device as fidl_wlan_dev;
use fuchsia_async::{self as fasync, unsafe_many_futures};
use fuchsia_zircon as zx;
use futures::channel::mpsc::UnboundedReceiver;
use futures::{future, Future, FutureExt, stream};
use futures::prelude::*;
use log::{error, info, log};
use std::sync::Arc;

use crate::device::{self, PhyDevice, PhyMap, IfaceDevice, IfaceMap};
use crate::Never;
use crate::station;
use crate::stats_scheduler::StatsRef;
use crate::watchable_map::MapEvent;
use crate::watcher_service;

unsafe_many_futures!(ServiceFut,
 [ListPhys, QueryPhy, ListIfaces, CreateIface, GetClientSme, GetIfaceStats, WatchDevices]);

const CONCURRENT_LIMIT: usize = 1000;

pub fn device_service<S>(phys: Arc<PhyMap>, ifaces: Arc<IfaceMap>,
                         phy_events: UnboundedReceiver<MapEvent<u16, PhyDevice>>,
                         iface_events: UnboundedReceiver<MapEvent<u16, IfaceDevice>>,
                         new_clients: S)
    -> impl Future<Output = Result<Never, failure::Error>>
    where S: Stream<Item = fasync::Channel>
{
    let (watcher_service, watcher_fut) = watcher_service::serve_watchers(
        phys.clone(), ifaces.clone(), phy_events, iface_events);
    let server = new_clients
        .map(Ok)
        .chain(stream::once(
            future::ready(
                Err(format_err!("new client stream in device_service ended unexpectedly")))))
        .try_for_each_concurrent(CONCURRENT_LIMIT, move |channel| {
            serve_channel(phys.clone(), ifaces.clone(), watcher_service.clone(), channel)
                .map(Ok)
        })
        .and_then(|_|
            future::ready(Err(format_err!("device_service server future exited unexpectedly"))));
    server.try_join(watcher_fut)
        .map_ok(|x: (Never, Never)| x.0)
}

fn serve_channel(phys: Arc<PhyMap>, ifaces: Arc<IfaceMap>,
                 watcher_service: watcher_service::WatcherService<PhyDevice, IfaceDevice>,
                 channel: fasync::Channel)
    -> impl Future<Output = ()>
{
    // Note that errors from responder.send() are propagated intentionally.
    // If we fail to send a response, the only way to recover is to stop serving the client
    // and close the channel. Otherwise, the client would be left hanging forever.
    fidl_svc::DeviceServiceRequestStream::from_channel(channel)
        .try_for_each_concurrent(CONCURRENT_LIMIT, move |request| match request {
            DeviceServiceRequest::ListPhys{ responder } => ServiceFut::ListPhys({
                future::ready(responder.send(&mut list_phys(&phys)))
            }),
            DeviceServiceRequest::QueryPhy { req, responder } => ServiceFut::QueryPhy({
                query_phy(&phys, req.phy_id)
                    .map(move |(status, mut r)| {
                        responder.send(status.into_raw(), r.as_mut().map(OutOfLine))
                    })
            }),
            DeviceServiceRequest::ListIfaces { responder } => ServiceFut::ListIfaces({
                future::ready(responder.send(&mut list_ifaces(&ifaces)))
            }),
            DeviceServiceRequest::CreateIface { req, responder } => ServiceFut::CreateIface({
                create_iface(&phys, req)
                    .map(move |(status, mut r)| {
                        responder.send(status.into_raw(), r.as_mut().map(OutOfLine))
                    })
            }),
            DeviceServiceRequest::DestroyIface{ req: _, responder: _ } => unimplemented!(),
            DeviceServiceRequest::GetClientSme{ iface_id, sme, responder } => ServiceFut::GetClientSme({
                let status = get_client_sme(&ifaces, iface_id, sme);
                future::ready(responder.send(status.into_raw()))
            }),
            DeviceServiceRequest::GetIfaceStats{ iface_id, responder } => ServiceFut::GetIfaceStats({
                get_iface_stats(&ifaces, iface_id)
                    .map(move |res| match res {
                        Ok(stats_ref) => {
                            let mut stats = stats_ref.lock();
                            responder.send(zx::sys::ZX_OK, Some(OutOfLine(&mut stats)))
                        }
                        Err(status) => responder.send(status.into_raw(), None)
                    })
            }),
            DeviceServiceRequest::WatchDevices{ watcher, control_handle: _ } => ServiceFut::WatchDevices({
                watcher_service.add_watcher(watcher)
                    .unwrap_or_else(|e| error!("error registering a device watcher: {}", e));
                future::ready(Ok(()))
            })
        })
        .map_ok(|_| ())
        .unwrap_or_else(|e| error!("error serving a DeviceService client: {}", e))
}

fn list_phys(phys: &Arc<PhyMap>) -> fidl_svc::ListPhysResponse {
    let list = phys
        .get_snapshot()
        .iter()
        .map(|(phy_id, phy)| {
            fidl_svc::PhyListItem {
                phy_id: *phy_id,
                path: phy.device.path().to_string_lossy().into_owned(),
            }
        })
        .collect();
    fidl_svc::ListPhysResponse { phys: list }
}

fn query_phy(phys: &Arc<PhyMap>, id: u16)
    -> impl Future<Output = (zx::Status, Option<fidl_svc::QueryPhyResponse>)>
{
    info!("query_phy(id = {})", id);
    let phy = phys.get(&id)
        .map(|phy| (phy.device.path().to_string_lossy().into_owned(), phy.proxy.clone()))
        .ok_or(zx::Status::NOT_FOUND);
    future::ready(phy)
        .and_then(move |(path, proxy)| {
            info!("query_phy(id = {}): sending 'Query' request to device", id);
            proxy.query()
                .map_err(move |e| {
                    error!("query_phy(id = {}): error sending 'Query' request to phy: {}", id, e);
                    zx::Status::INTERNAL
                })
                .and_then(move |query_result| {
                    info!("query_phy(id = {}): received a 'QueryResult' from device", id);
                    future::ready(zx::Status::ok(query_result.status)
                        .map(|()| {
                            let mut info = query_result.info;
                            info.id = id;
                            info.dev_path = Some(path);
                            info
                        }))
                })
        })
        .map(|r| match r {
            Ok(phy_info) => {
                let resp = fidl_svc::QueryPhyResponse { info: phy_info };
                (zx::Status::OK, Some(resp))
            },
            Err(status) => (status, None),
        })
}

fn list_ifaces(ifaces: &Arc<IfaceMap>) -> fidl_svc::ListIfacesResponse {
    let list = ifaces
        .get_snapshot()
        .iter()
        .map(|(iface_id, iface)| {
            fidl_svc::IfaceListItem {
                iface_id: *iface_id,
                path: iface.device.path().to_string_lossy().into_owned(),
            }
        })
        .collect();
    fidl_svc::ListIfacesResponse{ ifaces: list }
}

fn create_iface(phys: &Arc<PhyMap>, req: fidl_svc::CreateIfaceRequest)
    -> impl Future<Output = (zx::Status, Option<fidl_svc::CreateIfaceResponse>)>
{
    future::ready(phys.get(&req.phy_id)
        .map(|phy| phy.proxy.clone())
        .ok_or(zx::Status::NOT_FOUND))
        .and_then(move |proxy| {
            let mut phy_req = fidl_wlan_dev::CreateIfaceRequest { role: req.role };
            proxy.create_iface(&mut phy_req)
                .map_err(move |e| {
                    error!("Error sending 'CreateIface' request to phy #{}: {}", req.phy_id, e);
                    zx::Status::INTERNAL
                })
                .and_then(|r| future::ready(zx::Status::ok(r.status).map(move |()| r.iface_id)))
        })
        .map(|r| match r {
            Ok(iface_id) => {
                // TODO(gbonik): this is not the ID that we want to return
                let resp = fidl_svc::CreateIfaceResponse { iface_id };
                (zx::Status::OK, Some(resp))
            },
            Err(status) => (status, None),
        })
}

fn get_client_sme(ifaces: &Arc<IfaceMap>, iface_id: u16,
                  endpoint: station::ClientSmeEndpoint)
    -> zx::Status
{
    let iface = ifaces.get(&iface_id);
    let server = match iface {
        None => return zx::Status::NOT_FOUND,
        Some(ref iface) => match iface.sme_server {
            device::SmeServer::Client(ref server) => server,
            _ => return zx::Status::NOT_SUPPORTED
        }
    };
    match server.unbounded_send(endpoint) {
        Ok(()) => zx::Status::OK,
        Err(e) => {
            error!("error sending an endpoint to the SME server future: {}", e);
            zx::Status::INTERNAL
        }
    }
}

fn get_iface_stats(ifaces: &Arc<IfaceMap>, iface_id: u16)
    -> impl Future<Output = Result<StatsRef, zx::Status>>
{
    future::ready(ifaces.get(&iface_id)
        .ok_or(zx::Status::NOT_FOUND))
        .and_then(|iface| iface.stats_sched.get_stats())
}

#[cfg(test)]
mod tests {
    use super::*;

    use fidl::endpoints2::create_endpoints;
    use fidl_fuchsia_wlan_device::{PhyRequest, PhyRequestStream};
    use fidl_fuchsia_wlan_device_service::{IfaceListItem, PhyListItem};
    use fidl_fuchsia_wlan_sme as fidl_sme;
    use fuchsia_wlan_dev as wlan_dev;
    use futures::channel::mpsc;

    use crate::stats_scheduler::{self, StatsRequest};

    #[test]
    fn list_two_phys() {
        let _exec = fasync::Executor::new().expect("Failed to create an executor");
        let (phy_map, _phy_map_events) = PhyMap::new();
        let phy_map = Arc::new(phy_map);
        let (phy_null, _phy_null_stream) = fake_phy("/dev/null");
        let (phy_zero, _phy_zero_stream) = fake_phy("/dev/zero");
        phy_map.insert(10u16, phy_null);
        phy_map.insert(20u16, phy_zero);
        let mut list = super::list_phys(&phy_map).phys;
        list.sort_by_key(|p| p.phy_id);
        assert_eq!(vec![
            PhyListItem{ phy_id: 10u16, path: "/dev/null".to_string() },
            PhyListItem{ phy_id: 20u16, path: "/dev/zero".to_string() },
        ], list)
    }

    #[test]
    fn query_phy_success() {
        let mut exec = fasync::Executor::new().expect("Failed to create an executor");
        let (phy_map, _phy_map_events) = PhyMap::new();
        let phy_map = Arc::new(phy_map);
        let (phy, mut phy_stream) = fake_phy("/dev/null");
        phy_map.insert(10u16, phy);

        // Initiate a QueryPhy request. The returned future should not be able
        // to produce a result immediately
        let mut query_fut = super::query_phy(&phy_map, 10u16);
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut query_fut));

        // The call above should trigger a Query message to the phy.
        // Pretend that we are the phy and read the message from the other side.
        let responder = match exec.run_until_stalled(&mut phy_stream.next()) {
            Poll::Ready(Some(Ok(PhyRequest::Query{ responder }))) => responder,
            _ => panic!("phy_stream returned unexpected result")
        };

        // Reply with a fake phy info
        responder.send(&mut fidl_wlan_dev::QueryResponse {
            status: zx::sys::ZX_OK,
            info: fake_phy_info(),
        }).expect("failed to send QueryResponse");

        // Our original future should complete now, and return the same phy info
        let response = match exec.run_until_stalled(&mut query_fut) {
            Poll::Ready((zx::Status::OK, Some(response))) => response,
            other => panic!("query_fut returned unexpected result: {:?}", other),
        };
        assert_eq!(fake_phy_info(), response.info);
    }

    #[test]
    fn query_phy_not_found() {
        let mut exec = fasync::Executor::new().expect("Failed to create an executor");
        let (phy_map, _phy_map_events) = PhyMap::new();
        let phy_map = Arc::new(phy_map);

        let mut query_fut = super::query_phy(&phy_map, 10u16);
        assert_eq!(Poll::Ready((zx::Status::NOT_FOUND, None)),
                   exec.run_until_stalled(&mut query_fut));
    }

    #[test]
    fn list_two_ifaces() {
        let _exec = fasync::Executor::new().expect("Failed to create an executor");
        let (iface_map, _iface_map_events) = IfaceMap::new();
        let iface_map = Arc::new(iface_map);
        let iface_null = fake_client_iface("/dev/null");
        let iface_zero = fake_client_iface("/dev/zero");
        iface_map.insert(10u16, iface_null.iface);
        iface_map.insert(20u16, iface_zero.iface);
        let mut list = super::list_ifaces(&iface_map).ifaces;
        list.sort_by_key(|p| p.iface_id);
        assert_eq!(vec![
            IfaceListItem{ iface_id: 10u16, path: "/dev/null".to_string() },
            IfaceListItem{ iface_id: 20u16, path: "/dev/zero".to_string() },
        ], list)
    }

    #[test]
    fn create_iface_success() {
        let mut exec = fasync::Executor::new().expect("Failed to create an executor");
        let (phy_map, _phy_map_events) = PhyMap::new();
        let phy_map = Arc::new(phy_map);

        let (phy, mut phy_stream) = fake_phy("/dev/null");
        phy_map.insert(10u16, phy);

        // Initiate a CreateIface request. The returned future should not be able
        // to produce a result immediately
        let mut create_fut = super::create_iface(&phy_map, fidl_svc::CreateIfaceRequest {
            phy_id: 10,
            role: fidl_wlan_dev::MacRole::Client,
        });
        assert_eq!(Poll::Pending, exec.run_until_stalled(&mut create_fut));

        // The call above should trigger a CreateIface message to the phy.
        // Pretend that we are the phy and read the message from the other side.
        let (req, responder) = match exec.run_until_stalled(&mut phy_stream.next()) {
            Poll::Ready(Some(Ok(PhyRequest::CreateIface{ req, responder }))) =>
                (req, responder),
            _ => panic!("phy_stream returned unexpected result")
        };

        // Since we requested the Client role, the request to the phy should also have
        // the Client role
        assert_eq!(fidl_wlan_dev::MacRole::Client, req.role);

        // Pretend that we created an interface device with id 123 and send a response
        responder.send(&mut fidl_wlan_dev::CreateIfaceResponse {
            status: zx::sys::ZX_OK,
            iface_id: 123,
        }).expect("failed to send CreateIfaceResponse");

        // Now, our original future should resolve into a response
        let response = match exec.run_until_stalled(&mut create_fut) {
            Poll::Ready((zx::Status::OK, Some(response))) => response,
            other => panic!("create_fut returned unexpected result: {:?}", other),
        };
        // This assertion likely needs to change once we figure out a solution
        // to the iface id problem.
        assert_eq!(123, response.iface_id);
    }

    #[test]
    fn create_iface_not_found() {
        let mut exec = fasync::Executor::new().expect("Failed to create an executor");
        let (phy_map, _phy_map_events) = PhyMap::new();
        let phy_map = Arc::new(phy_map);

        let mut fut = super::create_iface(&phy_map, fidl_svc::CreateIfaceRequest {
            phy_id: 10,
            role: fidl_wlan_dev::MacRole::Client,
        });
        assert_eq!(Poll::Ready((zx::Status::NOT_FOUND, None)),
                   exec.run_until_stalled(&mut fut));
    }

    #[test]
    fn get_client_sme_success() {
        let mut exec = fasync::Executor::new().expect("Failed to create an executor");
        let (iface_map, _iface_map_events) = IfaceMap::new();
        let iface_map = Arc::new(iface_map);

        let mut iface = fake_client_iface("/dev/null");
        iface_map.insert(10, iface.iface);

        let (proxy, server) = create_endpoints().expect("failed to create a pair of SME endpoints");
        assert_eq!(zx::Status::OK, super::get_client_sme(&iface_map, 10, server));

        // Expect to get a new FIDL client in the stream
        let endpoint = iface.new_sme_clients.try_next()
            .expect("expected a message in new_sme_clients")
            .expect("didn't expect new_sme_clients stream to end");
        let mut sme_stream = endpoint.into_stream().expect("failed to create stream for endpoint");

        // Verify that `proxy` is indeed connected to `sme_stream`
        let (_scan_proxy, scan_txn) = create_endpoints()
            .expect("failed to create a pair of scan txn endpoints");
        proxy.scan(&mut fake_scan_request(), scan_txn).expect("failed to send a scan request");

        let req = match exec.run_until_stalled(&mut sme_stream.next()) {
            Poll::Ready(Some(Ok(fidl_sme::ClientSmeRequest::Scan{ req, .. }))) => req,
            _ => panic!("sme_stream returned unexpected result")
        };
        assert_eq!(fake_scan_request(), req);
    }

    #[test]
    fn get_client_sme_not_found() {
        let mut _exec = fasync::Executor::new().expect("Failed to create an executor");
        let (iface_map, _iface_map_events) = IfaceMap::new();
        let iface_map = Arc::new(iface_map);

        let (_proxy, server) = create_endpoints().expect("failed to create a pair of SME endpoints");
        assert_eq!(zx::Status::NOT_FOUND, super::get_client_sme(&iface_map, 10, server));
    }

    #[test]
    fn get_client_sme_wrong_role() {
        let mut _exec = fasync::Executor::new().expect("Failed to create an executor");
        let (iface_map, _iface_map_events) = IfaceMap::new();
        let iface_map = Arc::new(iface_map);

        let iface = fake_ap_iface("/dev/null");
        iface_map.insert(10, iface.iface);

        let (_proxy, server) = create_endpoints().expect("failed to create a pair of SME endpoints");
        assert_eq!(zx::Status::NOT_SUPPORTED, super::get_client_sme(&iface_map, 10, server));
    }

    fn fake_phy(path: &str) -> (PhyDevice, PhyRequestStream) {
        let (proxy, server) = create_endpoints::<fidl_wlan_dev::PhyMarker>()
            .expect("fake_phy: create_endpoints() failed");
        let device = wlan_dev::Device::new(path).expect(&format!("fake_phy: failed to open {}", path));
        let stream = server.into_stream().expect("fake_phy: failed to create stream");
        (PhyDevice { proxy, device }, stream)
    }

    struct FakeClientIface<St: Stream<Item = StatsRequest>> {
        iface: IfaceDevice,
        _stats_requests: St,
        new_sme_clients: mpsc::UnboundedReceiver<station::ClientSmeEndpoint>,
    }

    fn fake_client_iface(path: &str) -> FakeClientIface<impl Stream<Item = StatsRequest>> {
        let device = wlan_dev::Device::new(path)
            .expect(&format!("fake_client_iface: failed to open {}", path));
        let (sme_sender, sme_receiver) = mpsc::unbounded();
        let (stats_sched, stats_requests) = stats_scheduler::create_scheduler();
        let iface = IfaceDevice {
            sme_server: device::SmeServer::Client(sme_sender),
            stats_sched,
            device
        };
        FakeClientIface {
            iface,
            _stats_requests: stats_requests,
            new_sme_clients: sme_receiver
        }
    }

    struct FakeApIface<St: Stream<Item = StatsRequest>> {
        iface: IfaceDevice,
        _stats_requests: St,
    }

    fn fake_ap_iface(path: &str) -> FakeApIface<impl Stream<Item = StatsRequest>> {
        let device = wlan_dev::Device::new(path)
            .expect(&format!("fake_client_iface: failed to open {}", path));
        let (stats_sched, stats_requests) = stats_scheduler::create_scheduler();
        let iface = IfaceDevice {
            sme_server: device::SmeServer::_Ap,
            stats_sched,
            device
        };
        FakeApIface {
            iface,
            _stats_requests: stats_requests,
        }
    }

    fn fake_phy_info() -> fidl_wlan_dev::PhyInfo {
        fidl_wlan_dev::PhyInfo {
            id: 10,
            dev_path: Some("/dev/null".to_string()),
            hw_mac_address: [ 0x67, 0x62, 0x6f, 0x6e, 0x69, 0x6b ],
            supported_phys: Vec::new(),
            driver_features: Vec::new(),
            mac_roles: Vec::new(),
            caps: Vec::new(),
            bands: Vec::new(),
        }
    }

    fn fake_scan_request() -> fidl_sme::ScanRequest {
        fidl_sme::ScanRequest{
            timeout: 41
        }
    }
}
