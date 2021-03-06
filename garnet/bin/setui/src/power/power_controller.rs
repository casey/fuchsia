// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::registry::base::{Command, Context, SettingHandler},
    crate::registry::device_storage::DeviceStorageFactory,
    crate::service_context::ServiceContextHandle,
    crate::switchboard::base::*,
    fuchsia_async as fasync,
    futures::future::BoxFuture,
    futures::StreamExt,
};

async fn reboot(service_context_handle: ServiceContextHandle) {
    let device_admin = service_context_handle
        .lock()
        .await
        .connect::<fidl_fuchsia_device_manager::AdministratorMarker>()
        .await
        .expect("connected to device manager");

    device_admin.suspend(fidl_fuchsia_device_manager::SUSPEND_FLAG_REBOOT).await.ok();
}

pub fn spawn_power_controller<T: DeviceStorageFactory + Send + Sync + 'static>(
    context: Context<T>,
) -> BoxFuture<'static, SettingHandler> {
    let service_context_handle = context.environment.service_context_handle.clone();
    let (system_handler_tx, mut system_handler_rx) = futures::channel::mpsc::unbounded::<Command>();

    fasync::spawn(async move {
        while let Some(command) = system_handler_rx.next().await {
            match command {
                #[allow(unreachable_patterns)]
                Command::HandleRequest(request, responder) =>
                {
                    #[allow(unreachable_patterns)]
                    match request {
                        SettingRequest::Reboot => {
                            reboot(service_context_handle.clone()).await;
                            responder.send(Ok(None)).ok();
                        }
                        _ => {
                            responder
                                .send(Err(SwitchboardError::UnimplementedRequest {
                                    setting_type: SettingType::Power,
                                    request: request,
                                }))
                                .ok();
                        }
                    }
                }
                // Ignore unsupported commands
                _ => {}
            }
        }
    });
    Box::pin(async move { system_handler_tx })
}
