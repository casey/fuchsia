// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]
use {
    anyhow::Error,
    cobalt_sw_delivery_registry as metrics,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::DirectoryMarker,
    fidl_fuchsia_paver as paver,
    fidl_fuchsia_pkg::{PackageResolverRequestStream, PackageResolverResolveResponder},
    fidl_fuchsia_sys::{LauncherProxy, TerminationReason},
    fuchsia_async as fasync,
    fuchsia_component::{
        client::AppBuilder,
        server::{NestedEnvironment, ServiceFs},
    },
    fuchsia_zircon::{Status, Vmo},
    futures::prelude::*,
    parking_lot::Mutex,
    pretty_assertions::assert_eq,
    std::{
        collections::HashMap,
        convert::TryInto,
        fs::{create_dir, File},
        io::Write,
        path::{Path, PathBuf},
        sync::Arc,
    },
    tempfile::TempDir,
};

struct TestEnvBuilder {
    paver_service_builder: MockPaverServiceBuilder,
}

impl TestEnvBuilder {
    fn new() -> Self {
        TestEnvBuilder { paver_service_builder: MockPaverServiceBuilder::new() }
    }

    fn paver_service<F>(mut self, f: F) -> Self
    where
        F: FnOnce(MockPaverServiceBuilder) -> MockPaverServiceBuilder,
    {
        self.paver_service_builder = f(self.paver_service_builder);
        self
    }

    fn build(self) -> TestEnv {
        let paver_service = Arc::new(self.paver_service_builder.build());

        let mut fs = ServiceFs::new();
        fs.add_proxy_service::<fidl_fuchsia_logger::LogSinkMarker, _>();

        let resolver = Arc::new(MockResolverService::new());
        let resolver_clone = resolver.clone();
        fs.add_fidl_service(move |stream: PackageResolverRequestStream| {
            let resolver_clone = resolver_clone.clone();
            fasync::spawn(
                resolver_clone
                    .run_resolver_service(stream)
                    .unwrap_or_else(|e| panic!("error running resolver service: {:?}", e)),
            )
        });
        let paver_service_clone = paver_service.clone();
        fs.add_fidl_service(move |stream| {
            let paver_service_clone = paver_service_clone.clone();
            fasync::spawn(
                paver_service_clone
                    .run_paver_service(stream)
                    .unwrap_or_else(|e| panic!("error running paver service: {:?}", e)),
            )
        });
        let reboot_service = Arc::new(MockRebootService::new());
        let reboot_service_clone = reboot_service.clone();
        fs.add_fidl_service(move |stream| {
            let reboot_service_clone = reboot_service_clone.clone();
            fasync::spawn(
                reboot_service_clone
                    .run_reboot_service(stream)
                    .unwrap_or_else(|e| panic!("error running reboot service: {:?}", e)),
            )
        });
        let logger_factory = Arc::new(MockLoggerFactory::new());
        let logger_factory_clone = logger_factory.clone();
        fs.add_fidl_service(move |stream| {
            let logger_factory_clone = logger_factory_clone.clone();
            fasync::spawn(
                logger_factory_clone
                    .run_logger_factory(stream)
                    .unwrap_or_else(|e| panic!("error running logger factory: {:?}", e)),
            )
        });
        let space_service = Arc::new(MockSpaceService::new());
        let space_service_clone = space_service.clone();
        fs.add_fidl_service(move |stream| {
            let space_service_clone = space_service_clone.clone();
            fasync::spawn(
                space_service_clone
                    .run_space_service(stream)
                    .unwrap_or_else(|e| panic!("error running space service: {:?}", e)),
            )
        });
        let env = fs
            .create_salted_nested_environment("systemupdater_env")
            .expect("nested environment to create successfully");
        fasync::spawn(fs.collect());

        let test_dir = TempDir::new().expect("create test tempdir");

        let blobfs_path = test_dir.path().join("blob");
        create_dir(&blobfs_path).expect("create blob dir");

        let packages_path = test_dir.path().join("packages");
        create_dir(&packages_path).expect("create packages dir");

        let fake_path = test_dir.path().join("fake");
        create_dir(&fake_path).expect("create fake stimulus dir");

        let config_path = test_dir.path().join("config");
        create_dir(&config_path).expect("create config dir");

        TestEnv {
            env,
            resolver,
            paver_service,
            reboot_service,
            logger_factory,
            space_service,
            _test_dir: test_dir,
            packages_path,
            blobfs_path,
            fake_path,
            config_path,
        }
    }
}

struct TestEnv {
    env: NestedEnvironment,
    resolver: Arc<MockResolverService>,
    paver_service: Arc<MockPaverService>,
    reboot_service: Arc<MockRebootService>,
    logger_factory: Arc<MockLoggerFactory>,
    space_service: Arc<MockSpaceService>,
    _test_dir: TempDir,
    packages_path: PathBuf,
    blobfs_path: PathBuf,
    fake_path: PathBuf,
    config_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        Self::builder().build()
    }

    fn builder() -> TestEnvBuilder {
        TestEnvBuilder::new()
    }

    fn launcher(&self) -> &LauncherProxy {
        self.env.launcher()
    }

    /// Set the name of the board that system_updater is running on.
    fn set_board_name(&self, board: impl AsRef<str>) {
        // Create the /config/board-info directory.
        let build_info_dir = self.config_path.join("build-info");
        create_dir(&build_info_dir).expect("create build-info dir");

        // Write the "board" file into the directory.
        let mut file = File::create(build_info_dir.join("board")).expect("create board file");
        file.write_all(board.as_ref().as_bytes()).expect("write board file");
    }

    fn register_package(&mut self, name: impl AsRef<str>, merkle: impl AsRef<str>) -> TestPackage {
        let name = name.as_ref();
        let merkle = merkle.as_ref();

        let root = self.packages_path.join(merkle);
        create_dir(&root).expect("package to not yet exist");

        self.resolver
            .mock_package_result(format!("fuchsia-pkg://fuchsia.com/{}", name), Ok(root.clone()));

        TestPackage { root }.add_file("meta", merkle)
    }

    async fn run_system_updater<'a>(
        &'a self,
        args: SystemUpdaterArgs<'a>,
    ) -> Result<(), fuchsia_component::client::OutputError> {
        let mut v =
            vec![format!("-initiator={}", args.initiator), format!("-target={}", args.target)];

        if let Some(update) = args.update {
            v.push(format!("-update={}", update));
        }
        if let Some(reboot) = args.reboot {
            v.push(format!("-reboot={}", reboot));
        }

        self.run_system_updater_args(v).await
    }

    async fn run_system_updater_args<'a>(
        &'a self,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), fuchsia_component::client::OutputError> {
        let launcher = self.launcher();
        let blobfs_dir = File::open(&self.blobfs_path).expect("open blob dir");
        let fake_dir = File::open(&self.fake_path).expect("open fake stimulus dir");
        let config_dir = File::open(&self.config_path).expect("open config dir");

        let system_updater = AppBuilder::new(
            "fuchsia-pkg://fuchsia.com/system-updater-integration-tests#meta/system_updater_isolated.cmx",
        )
        .add_dir_to_namespace("/blob".to_string(), blobfs_dir)
        .expect("/blob to mount")
        .add_dir_to_namespace("/fake".to_string(), fake_dir)
        .expect("/fake to mount")
        .add_dir_to_namespace("/config".to_string(), config_dir)
        .expect("/config to mount")
        .args(args);

        let output = system_updater
            .output(launcher)
            .expect("system_updater to launch")
            .await
            .expect("no errors while waiting for exit");

        if !output.stdout.is_empty() {
            eprintln!("TEST: system updater stdout:\n{}", String::from_utf8_lossy(&output.stdout));
        }

        if !output.stderr.is_empty() {
            eprintln!("TEST: system updater stderr:\n{}", String::from_utf8_lossy(&output.stderr));
        }

        assert_eq!(output.exit_status.reason(), TerminationReason::Exited);
        output.ok()
    }
}

struct TestPackage {
    root: PathBuf,
}

impl TestPackage {
    fn add_file(self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Self {
        std::fs::write(self.root.join(path), contents).expect("create fake package file");
        self
    }
}

struct SystemUpdaterArgs<'a> {
    initiator: &'a str,
    target: &'a str,
    update: Option<&'a str>,
    reboot: Option<bool>,
}

struct MockResolverService {
    resolved_urls: Mutex<Vec<String>>,
    expectations: Mutex<HashMap<String, Result<PathBuf, Status>>>,
}

impl MockResolverService {
    fn new() -> Self {
        Self { resolved_urls: Mutex::new(vec![]), expectations: Mutex::new(HashMap::new()) }
    }
    async fn run_resolver_service(
        self: Arc<Self>,
        mut stream: PackageResolverRequestStream,
    ) -> Result<(), Error> {
        while let Some(event) = stream.try_next().await? {
            match event {
                fidl_fuchsia_pkg::PackageResolverRequest::Resolve {
                    package_url,
                    selectors: _,
                    update_policy: _,
                    dir,
                    responder,
                } => self.handle_resolve(package_url, dir, responder).await?,
                fidl_fuchsia_pkg::PackageResolverRequest::GetHash {
                    package_url: _,
                    responder: _,
                } => panic!("GetHash not implemented"),
            }
        }
        Ok(())
    }
    async fn handle_resolve(
        &self,
        package_url: String,
        dir: ServerEnd<DirectoryMarker>,
        responder: PackageResolverResolveResponder,
    ) -> Result<(), Error> {
        eprintln!("TEST: Got resolve request for {:?}", package_url);

        let response = self
            .expectations
            .lock()
            .get(&package_url)
            .map(|entry| entry.clone())
            // Successfully resolve unexpected packages without serving a package dir. Log the
            // transaction so tests can decide if it was expected.
            .unwrap_or(Err(Status::OK));
        self.resolved_urls.lock().push(package_url);

        let response_status = match response {
            Ok(package_dir) => {
                // Open the package directory using the directory request given by the client
                // asking to resolve the package.
                fdio::service_connect(
                    package_dir.to_str().expect("path to str"),
                    dir.into_channel(),
                )
                .unwrap_or_else(|err| panic!("error connecting to tempdir {:?}", err));
                Status::OK
            }
            Err(status) => status,
        };
        responder.send(response_status.into_raw())?;
        Ok(())
    }

    fn mock_package_result(&self, url: impl Into<String>, response: Result<PathBuf, Status>) {
        self.expectations.lock().insert(url.into(), response);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PaverEvent {
    WriteAsset { configuration: paver::Configuration, asset: paver::Asset, payload: Vec<u8> },
    WriteBootloader(Vec<u8>),
    QueryActiveConfiguration,
    SetConfigurationActive { configuration: paver::Configuration },
}

struct MockPaverServiceBuilder {
    call_hook: Option<Box<dyn Fn(&PaverEvent) -> Status + Send + Sync>>,
    active_config: paver::Configuration,
    boot_manager_close_with_epitaph: Option<Status>,
}

impl MockPaverServiceBuilder {
    fn new() -> Self {
        Self {
            call_hook: None,
            active_config: paver::Configuration::A,
            boot_manager_close_with_epitaph: None,
        }
    }

    fn call_hook<F>(mut self, call_hook: F) -> Self
    where
        F: Fn(&PaverEvent) -> Status + Send + Sync + 'static,
    {
        self.call_hook = Some(Box::new(call_hook));
        self
    }

    fn active_config(mut self, active_config: paver::Configuration) -> Self {
        self.active_config = active_config;
        self
    }

    fn boot_manager_close_with_epitaph(mut self, status: Status) -> Self {
        self.boot_manager_close_with_epitaph = Some(status);
        self
    }

    fn build(self) -> MockPaverService {
        let call_hook = self.call_hook.unwrap_or_else(|| Box::new(|_| Status::OK));

        MockPaverService {
            events: Mutex::new(vec![]),
            call_hook: Box::new(call_hook),
            active_config: self.active_config,
            boot_manager_close_with_epitaph: self.boot_manager_close_with_epitaph,
        }
    }
}

struct MockPaverService {
    events: Mutex<Vec<PaverEvent>>,
    call_hook: Box<dyn Fn(&PaverEvent) -> Status + Send + Sync>,
    active_config: paver::Configuration,
    boot_manager_close_with_epitaph: Option<Status>,
}

impl MockPaverService {
    fn take_events(&self) -> Vec<PaverEvent> {
        std::mem::replace(&mut *self.events.lock(), vec![])
    }

    async fn run_data_sink_service(
        self: Arc<Self>,
        mut stream: paver::DataSinkRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                paver::DataSinkRequest::WriteAsset {
                    configuration,
                    asset,
                    mut payload,
                    responder,
                } => {
                    let payload = verify_and_read_buffer(&mut payload);
                    let event = PaverEvent::WriteAsset { configuration, asset, payload };
                    let status = (*self.call_hook)(&event);
                    self.events.lock().push(event);
                    responder.send(status.into_raw()).expect("paver response to send");
                }
                paver::DataSinkRequest::WriteBootloader { mut payload, responder } => {
                    let payload = verify_and_read_buffer(&mut payload);
                    let event = PaverEvent::WriteBootloader(payload);
                    let status = (*self.call_hook)(&event);
                    self.events.lock().push(event);
                    responder.send(status.into_raw()).expect("paver response to send");
                }
                request => panic!("Unhandled method Paver::{}", request.method_name()),
            }
        }

        Ok(())
    }

    async fn run_boot_manager_service(
        self: Arc<Self>,
        boot_manager: fidl::endpoints::ServerEnd<paver::BootManagerMarker>,
    ) -> Result<(), Error> {
        if let Some(status) = self.boot_manager_close_with_epitaph {
            boot_manager.close_with_epitaph(status)?;
            return Ok(());
        };

        let mut stream = boot_manager.into_stream()?;

        while let Some(request) = stream.try_next().await? {
            match request {
                paver::BootManagerRequest::QueryActiveConfiguration { responder } => {
                    let event = PaverEvent::QueryActiveConfiguration;
                    let status = (*self.call_hook)(&event);
                    self.events.lock().push(event);
                    let mut result = if status == Status::OK {
                        Ok(self.active_config)
                    } else {
                        Err(status.into_raw())
                    };
                    responder.send(&mut result).expect("paver response to send");
                }
                paver::BootManagerRequest::SetConfigurationActive { configuration, responder } => {
                    let event = PaverEvent::SetConfigurationActive { configuration };
                    let status = (*self.call_hook)(&event);
                    self.events.lock().push(event);
                    responder.send(status.into_raw()).expect("paver response to send");
                }
                request => panic!("Unhandled method Paver::{}", request.method_name()),
            }
        }

        Ok(())
    }

    async fn run_paver_service(
        self: Arc<Self>,
        mut stream: paver::PaverRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                paver::PaverRequest::FindDataSink { data_sink, .. } => {
                    let paver_service_clone = self.clone();
                    fasync::spawn(
                        paver_service_clone
                            .run_data_sink_service(data_sink.into_stream()?)
                            .unwrap_or_else(|e| panic!("error running data sink service: {:?}", e)),
                    );
                }
                paver::PaverRequest::FindBootManager { boot_manager, .. } => {
                    let paver_service_clone = self.clone();
                    fasync::spawn(
                        paver_service_clone.run_boot_manager_service(boot_manager).unwrap_or_else(
                            |e| panic!("error running boot manager service: {:?}", e),
                        ),
                    );
                }
                request => panic!("Unhandled method Paver::{}", request.method_name()),
            }
        }

        Ok(())
    }
}

fn verify_and_read_buffer(buffer: &mut fidl_fuchsia_mem::Buffer) -> Vec<u8> {
    // The paver service requires VMOs to be resizable. Assert that the buffer provided by the
    // system updater can be resized without error.
    resize_vmo(&mut buffer.vmo);
    read_mem_buffer(buffer)
}

fn resize_vmo(vmo: &mut Vmo) {
    let size = vmo.get_size().expect("vmo size query to succeed");
    vmo.set_size(size * 2).expect("vmo must be resizable");
}

fn read_mem_buffer(buffer: &fidl_fuchsia_mem::Buffer) -> Vec<u8> {
    let mut res = vec![0; buffer.size.try_into().expect("usize")];
    buffer.vmo.read(&mut res[..], 0).expect("vmo read to succeed");
    res
}

struct MockRebootService {
    called: Mutex<u32>,
}
impl MockRebootService {
    fn new() -> Self {
        Self { called: Mutex::new(0) }
    }

    async fn run_reboot_service(
        self: Arc<Self>,
        mut stream: fidl_fuchsia_device_manager::AdministratorRequestStream,
    ) -> Result<(), Error> {
        while let Some(event) = stream.try_next().await? {
            let fidl_fuchsia_device_manager::AdministratorRequest::Suspend { flags, responder } =
                event;
            eprintln!("TEST: Got reboot request with flags {:?}", flags);
            *self.called.lock() += 1;
            responder.send(Status::OK.into_raw())?;
        }

        Ok(())
    }
}

#[derive(Clone)]
struct CustomEvent {
    metric_id: u32,
    values: Vec<fidl_fuchsia_cobalt::CustomEventValue>,
}

struct MockLogger {
    cobalt_events: Mutex<Vec<fidl_fuchsia_cobalt::CobaltEvent>>,
}

impl MockLogger {
    fn new() -> Self {
        Self { cobalt_events: Mutex::new(vec![]) }
    }

    async fn run_logger(
        self: Arc<Self>,
        mut stream: fidl_fuchsia_cobalt::LoggerRequestStream,
    ) -> Result<(), Error> {
        while let Some(event) = stream.try_next().await? {
            match event {
                fidl_fuchsia_cobalt::LoggerRequest::LogCobaltEvent { event, responder } => {
                    self.cobalt_events.lock().push(event);
                    responder.send(fidl_fuchsia_cobalt::Status::Ok)?;
                }
                _ => {
                    panic!("unhandled Logger method {:?}", event);
                }
            }
        }

        Ok(())
    }
}

struct MockLoggerFactory {
    loggers: Mutex<Vec<Arc<MockLogger>>>,
    broken: Mutex<bool>,
}

impl MockLoggerFactory {
    fn new() -> Self {
        Self { loggers: Mutex::new(vec![]), broken: Mutex::new(false) }
    }

    async fn run_logger_factory(
        self: Arc<Self>,
        mut stream: fidl_fuchsia_cobalt::LoggerFactoryRequestStream,
    ) -> Result<(), Error> {
        if *self.broken.lock() {
            eprintln!("TEST: This LoggerFactory is broken by order of the test.");
            // Drop the stream, closing the channel.
            return Ok(());
        }
        while let Some(event) = stream.try_next().await? {
            match event {
                fidl_fuchsia_cobalt::LoggerFactoryRequest::CreateLoggerFromProjectId {
                    project_id,
                    logger,
                    responder,
                } => {
                    eprintln!("TEST: Got CreateLogger request with project_id {:?}", project_id);
                    let mock_logger = Arc::new(MockLogger::new());
                    self.loggers.lock().push(mock_logger.clone());
                    fasync::spawn(
                        mock_logger
                            .run_logger(logger.into_stream()?)
                            .unwrap_or_else(|e| eprintln!("error while running Logger: {:?}", e)),
                    );
                    responder.send(fidl_fuchsia_cobalt::Status::Ok)?;
                }
                _ => {
                    panic!("unhandled LoggerFactory method: {:?}", event);
                }
            }
        }

        Ok(())
    }
}

struct MockSpaceService {
    called: Mutex<u32>,
}
impl MockSpaceService {
    fn new() -> Self {
        Self { called: Mutex::new(0) }
    }

    async fn run_space_service(
        self: Arc<Self>,
        mut stream: fidl_fuchsia_space::ManagerRequestStream,
    ) -> Result<(), Error> {
        while let Some(event) = stream.try_next().await? {
            let fidl_fuchsia_space::ManagerRequest::Gc { responder } = event;
            *self.called.lock() += 1;
            responder.send(&mut Ok(()))?;
        }

        Ok(())
    }
}

#[derive(PartialEq, Eq, Debug)]
struct OtaMetrics {
    initiator: u32,
    phase: u32,
    status_code: u32,
    target: String,
    // TODO: support free_space_delta assertions
}

impl OtaMetrics {
    fn from_events(mut events: Vec<fidl_fuchsia_cobalt::CobaltEvent>) -> Self {
        events.sort_by_key(|e| e.metric_id);

        // expecting one of each event
        assert_eq!(
            events.iter().map(|e| e.metric_id).collect::<Vec<_>>(),
            vec![
                metrics::OTA_START_METRIC_ID,
                metrics::OTA_RESULT_ATTEMPTS_METRIC_ID,
                metrics::OTA_RESULT_DURATION_METRIC_ID,
                metrics::OTA_RESULT_FREE_SPACE_DELTA_METRIC_ID
            ]
        );

        // we just asserted that we have the exact 4 things we're expecting, so unwrap them
        let mut iter = events.into_iter();
        let start = iter.next().unwrap();
        let attempt = iter.next().unwrap();
        let duration = iter.next().unwrap();
        let free_space_delta = iter.next().unwrap();

        // Some basic sanity checks follow
        assert_eq!(
            attempt.payload,
            fidl_fuchsia_cobalt::EventPayload::EventCount(fidl_fuchsia_cobalt::CountEvent {
                period_duration_micros: 0,
                count: 1
            })
        );

        let fidl_fuchsia_cobalt::CobaltEvent { event_codes, component, .. } = attempt;

        // metric event_codes and component should line up across all 3 result metrics
        assert_eq!(&duration.event_codes, &event_codes);
        assert_eq!(&duration.component, &component);
        assert_eq!(&free_space_delta.event_codes, &event_codes);
        assert_eq!(&free_space_delta.component, &component);

        // OtaStart only has initiator and hour_of_day, so just check initiator.
        assert_eq!(start.event_codes[0], event_codes[0]);
        assert_eq!(&start.component, &component);

        let target = component.expect("a target update merkle");

        assert_eq!(event_codes.len(), 3);
        let initiator = event_codes[0];
        let phase = event_codes[1];
        let status_code = event_codes[2];

        match duration.payload {
            fidl_fuchsia_cobalt::EventPayload::ElapsedMicros(_time) => {
                // Ignore the value since timing is not predictable.
            }
            other => {
                panic!("unexpected duration payload {:?}", other);
            }
        }

        // Ignore this for now, since it's using a shared tempdir, the values
        // are not deterministic.
        let _free_space_delta = match free_space_delta.payload {
            fidl_fuchsia_cobalt::EventPayload::EventCount(fidl_fuchsia_cobalt::CountEvent {
                period_duration_micros: 0,
                count,
            }) => count,
            other => {
                panic!("unexpected free space delta payload {:?}", other);
            }
        };

        Self { initiator, phase, status_code, target }
    }
}

#[fasync::run_singlethreaded(test)]
async fn test_system_update() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("run system_updater");

    assert_eq!(*env.resolver.resolved_urls.lock(), vec![
        "fuchsia-pkg://fuchsia.com/update",
        "fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296",
    ]);

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::SuccessPendingReboot as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Success as u32,
            target: "m3rk13".into(),
        }
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_packages_json_takes_precedence() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file(
            "packages.json",
            &serde_json::json!({
              "version": 1,
              "content": [
                "fuchsia-pkg://fuchsia.com/amber/0?hash=abcdef",
                "fuchsia-pkg://fuchsia.com/pkgfs/0?hash=123456789",
              ]
            })
            .to_string(),
        )
        .add_file("zbi", "fake zbi");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("run system_updater");

    assert_eq!(
        *env.resolver.resolved_urls.lock(),
        vec![
            "fuchsia-pkg://fuchsia.com/update",
            "fuchsia-pkg://fuchsia.com/amber/0?hash=abcdef",
            "fuchsia-pkg://fuchsia.com/pkgfs/0?hash=123456789"
        ]
    );
}

#[fasync::run_singlethreaded(test)]
async fn test_metrics_report_untrusted_tuf_repo() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages.json",
            "
          {
            \"version\": 1,
            \"content\": [
              \"fuchsia-pkg://non-existent-repo.com/amber/0?hash=abcdef\",
              \"fuchsia-pkg://fuchsia.com/pkgfs/0?hash=123456789\"
              ]
          }
        ",
        )
        .add_file("zbi", "fake zbi");

    env.resolver.mock_package_result(
        "fuchsia-pkg://non-existent-repo.com/amber/0?hash=abcdef",
        Err(Status::ADDRESS_UNREACHABLE),
    );

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: None,
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::PackageDownload as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::ErrorUntrustedTufRepo
                as u32,
            target: "m3rk13".into(),
        }
    );
}

#[fasync::run_singlethreaded(test)]
async fn test_system_update_no_reboot() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: Some(false),
    })
    .await
    .expect("run system_updater");

    assert_eq!(*env.resolver.resolved_urls.lock(), vec![
        "fuchsia-pkg://fuchsia.com/update",
        "fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296",
    ]);

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::SuccessPendingReboot as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Success as u32,
            target: "m3rk13".into(),
        }
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 0);
}

#[fasync::run_singlethreaded(test)]
async fn test_broken_logger() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi");

    *env.logger_factory.broken.lock() = true;

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("run system_updater");

    assert_eq!(*env.resolver.resolved_urls.lock(), vec![
        "fuchsia-pkg://fuchsia.com/update",
        "fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296"
    ]);

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 0);

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_failing_package_fetch() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3").add_file(
        "packages",
        "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
    );

    env.resolver.mock_package_result("fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296", Err(Status::NOT_FOUND));

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: None,
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    assert_eq!(*env.resolver.resolved_urls.lock(), vec![
        "fuchsia-pkg://fuchsia.com/update",
        "fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296"
    ]);

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::PackageDownload as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Error as u32,
            target: "m3rk13".into(),
        }
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 0);
}

#[fasync::run_singlethreaded(test)]
async fn test_requires_zbi() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("bootloader", "new bootloader");

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: None,
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    assert_eq!(*env.space_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_validate_board() {
    let mut env = TestEnv::new();

    env.set_board_name("x64");

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("board", "x64")
        .add_file("zbi", "fake zbi")
        .add_file("bootloader", "new bootloader");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    assert_eq!(*env.resolver.resolved_urls.lock(), vec![
        "fuchsia-pkg://fuchsia.com/update",
        "fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296",
    ]);
}

#[fasync::run_singlethreaded(test)]
async fn test_invalid_board() {
    let mut env = TestEnv::new();

    env.set_board_name("x64");

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("board", "arm")
        .add_file("zbi", "fake zbi")
        .add_file("bootloader", "new bootloader");

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: None,
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    // Expect to have failed prior to downloading images.
    assert_eq!(*env.resolver.resolved_urls.lock(), vec!["fuchsia-pkg://fuchsia.com/update"]);

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: 0u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Error as u32,
            target: "m3rk13".into(),
        }
    );
}

#[fasync::run_singlethreaded(test)]
async fn test_writes_bootloader() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi")
        .add_file("bootloader", "new bootloader");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    assert_eq!(
        env.paver_service.take_events(),
        vec![
            PaverEvent::QueryActiveConfiguration,
            PaverEvent::WriteBootloader(b"new bootloader".to_vec(),),
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::B,
                asset: paver::Asset::Kernel,
                payload: b"fake zbi".to_vec(),
            },
            PaverEvent::SetConfigurationActive { configuration: paver::Configuration::B },
        ]
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_writes_recovery() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi")
        .add_file("zedboot", "new recovery");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    assert_eq!(
        env.paver_service.take_events(),
        vec![
            PaverEvent::QueryActiveConfiguration,
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::B,
                asset: paver::Asset::Kernel,
                payload: b"fake zbi".to_vec(),
            },
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::Recovery,
                asset: paver::Asset::Kernel,
                payload: b"new recovery".to_vec(),
            },
            PaverEvent::SetConfigurationActive { configuration: paver::Configuration::B },
        ]
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_writes_recovery_vbmeta() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi")
        .add_file("zedboot", "new recovery")
        .add_file("recovery.vbmeta", "new recovery vbmeta");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    assert_eq!(
        env.paver_service.take_events(),
        vec![
            PaverEvent::QueryActiveConfiguration,
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::B,
                asset: paver::Asset::Kernel,
                payload: b"fake zbi".to_vec(),
            },
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::Recovery,
                asset: paver::Asset::Kernel,
                payload: b"new recovery".to_vec(),
            },
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::Recovery,
                asset: paver::Asset::VerifiedBootMetadata,
                payload: b"new recovery vbmeta".to_vec(),
            },
            PaverEvent::SetConfigurationActive { configuration: paver::Configuration::B },
        ]
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_writes_fuchsia_vbmeta() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi")
        .add_file("fuchsia.vbmeta", "fake zbi vbmeta");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    assert_eq!(
        env.paver_service.take_events(),
        vec![
            PaverEvent::QueryActiveConfiguration,
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::B,
                asset: paver::Asset::Kernel,
                payload: b"fake zbi".to_vec(),
            },
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::B,
                asset: paver::Asset::VerifiedBootMetadata,
                payload: b"fake zbi vbmeta".to_vec(),
            },
            PaverEvent::SetConfigurationActive { configuration: paver::Configuration::B },
        ]
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

async fn do_test_working_image_write_with_abr(
    active_config: paver::Configuration,
    target_config: paver::Configuration,
) {
    let mut env =
        TestEnv::builder().paver_service(|builder| builder.active_config(active_config)).build();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake_zbi");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::SuccessPendingReboot as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Success as u32,
            target: "m3rk13".into(),
        }
    );

    assert_eq!(
        env.paver_service.take_events(),
        vec![
            PaverEvent::QueryActiveConfiguration,
            PaverEvent::WriteAsset {
                configuration: target_config,
                asset: paver::Asset::Kernel,
                payload: b"fake_zbi".to_vec(),
            },
            PaverEvent::SetConfigurationActive { configuration: target_config },
        ]
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_working_image_write_with_abr_and_active_config_a() {
    do_test_working_image_write_with_abr(paver::Configuration::A, paver::Configuration::B).await
}

#[fasync::run_singlethreaded(test)]
async fn test_working_image_write_with_abr_and_active_config_b() {
    do_test_working_image_write_with_abr(paver::Configuration::B, paver::Configuration::A).await
}

#[fasync::run_singlethreaded(test)]
async fn test_working_image_with_unsupported_abr() {
    let mut env = TestEnv::builder()
        .paver_service(|builder| builder.boot_manager_close_with_epitaph(Status::NOT_SUPPORTED))
        .build();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake_zbi");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: None,
        reboot: None,
    })
    .await
    .expect("success");

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::SuccessPendingReboot as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Success as u32,
            target: "m3rk13".into(),
        }
    );

    assert_eq!(
        env.paver_service.take_events(),
        vec![
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::A,
                asset: paver::Asset::Kernel,
                payload: b"fake_zbi".to_vec(),
            },
            PaverEvent::WriteAsset {
                configuration: paver::Configuration::B,
                asset: paver::Asset::Kernel,
                payload: b"fake_zbi".to_vec(),
            }
        ]
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_failing_image_write() {
    let mut env =
        TestEnv::builder().paver_service(|builder| builder.call_hook(|_| Status::INTERNAL)).build();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake_zbi");

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: None,
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    let loggers = env.logger_factory.loggers.lock().clone();
    assert_eq!(loggers.len(), 1);
    let logger = loggers.into_iter().next().unwrap();
    assert_eq!(
        OtaMetrics::from_events(logger.cobalt_events.lock().clone()),
        OtaMetrics {
            initiator: metrics::OtaResultAttemptsMetricDimensionInitiator::UserInitiatedCheck
                as u32,
            phase: metrics::OtaResultAttemptsMetricDimensionPhase::ImageWrite as u32,
            status_code: metrics::OtaResultAttemptsMetricDimensionStatusCode::Error as u32,
            target: "m3rk13".into(),
        }
    );

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.reboot_service.called.lock(), 0);
}

#[fasync::run_singlethreaded(test)]
async fn test_uses_custom_update_package() {
    let mut env = TestEnv::new();

    env.register_package("another-update/4", "upd4t3r")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi");

    env.run_system_updater(SystemUpdaterArgs {
        initiator: "manual",
        target: "m3rk13",
        update: Some("fuchsia-pkg://fuchsia.com/another-update/4"),
        reboot: None,
    })
    .await
    .expect("run system_updater");

    assert_eq!(*env.resolver.resolved_urls.lock(), vec![
        "fuchsia-pkg://fuchsia.com/another-update/4",
        "fuchsia-pkg://fuchsia.com/system_image/0?hash=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296",
    ]);

    assert_eq!(*env.space_service.called.lock(), 1);
}

#[fasync::run_singlethreaded(test)]
async fn test_requires_update_package() {
    let env = TestEnv::new();

    env.resolver.mock_package_result("fuchsia-pkg://fuchsia.com/update", Err(Status::NOT_FOUND));

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: None,
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.resolver.resolved_urls.lock(), vec!["fuchsia-pkg://fuchsia.com/update"]);
    assert_eq!(*env.reboot_service.called.lock(), 0);
}

#[fasync::run_singlethreaded(test)]
async fn test_rejects_invalid_update_package_url() {
    let env = TestEnv::new();

    let bogus_url = "not-fuchsia-pkg://fuchsia.com/not-a-update";

    env.resolver.mock_package_result(bogus_url, Err(Status::INVALID_ARGS));

    let result = env
        .run_system_updater(SystemUpdaterArgs {
            initiator: "manual",
            target: "m3rk13",
            update: Some(bogus_url),
            reboot: None,
        })
        .await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    assert_eq!(*env.space_service.called.lock(), 1);
    assert_eq!(*env.resolver.resolved_urls.lock(), vec![bogus_url]);
    assert_eq!(*env.reboot_service.called.lock(), 0);
}

#[fasync::run_singlethreaded(test)]
async fn test_rejects_unknown_flags() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi");

    let result =
        env.run_system_updater_args(vec!["-initiator=manual", "-target=m3rk13", "-foo=bar"]).await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    assert_eq!(*env.space_service.called.lock(), 0);
    assert_eq!(*env.resolver.resolved_urls.lock(), Vec::<String>::new());
    assert_eq!(*env.reboot_service.called.lock(), 0);
}

#[fasync::run_singlethreaded(test)]
async fn test_rejects_extra_args() {
    let mut env = TestEnv::new();

    env.register_package("update", "upd4t3")
        .add_file(
            "packages",
            "system_image/0=42ade6f4fd51636f70c68811228b4271ed52c4eb9a647305123b4f4d0741f296\n",
        )
        .add_file("zbi", "fake zbi");

    let result =
        env.run_system_updater_args(vec!["-initiator=manual", "-target=m3rk13", "foo"]).await;
    assert!(result.is_err(), "system_updater succeeded when it should fail");

    assert_eq!(*env.space_service.called.lock(), 0);
    assert_eq!(*env.resolver.resolved_urls.lock(), Vec::<String>::new());
    assert_eq!(*env.reboot_service.called.lock(), 0);
}
