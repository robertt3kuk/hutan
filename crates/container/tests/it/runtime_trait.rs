//! The `ContainerRuntime` trait is implementable without an executor and its `exec_*` defaults
//! build launches from the runtime's CLI.

use std::time::Duration;

use hutan_container::detect::detect_apple;
use hutan_container::{
    Cli, ContainerId, ContainerRef, ContainerRuntime, ContainerSpec, ContainerState, Dialect,
    ExecSpec, HostProbe, RuntimeCaps, RuntimeError, RuntimeId, RuntimeKind, RuntimeReport,
    RuntimeState, VolumeName,
};

use crate::common::fake::FakeProbe;
use crate::common::strs;

struct Scripted {
    id: RuntimeId,
    cli: Cli,
}

impl Scripted {
    fn apple() -> Self {
        Self {
            id: RuntimeId::of(RuntimeKind::AppleContainer),
            cli: Cli::new(Dialect::Apple, "/usr/local/bin/container"),
        }
    }
}

impl ContainerRuntime for Scripted {
    fn id(&self) -> &RuntimeId {
        &self.id
    }
    fn cli(&self) -> &Cli {
        &self.cli
    }
    fn caps(&self) -> RuntimeCaps {
        RuntimeCaps::for_kind(RuntimeKind::AppleContainer)
    }
    fn detect(&self, probe: &dyn HostProbe) -> RuntimeReport {
        detect_apple(probe)
    }
    fn ensure(
        &self,
        spec: &ContainerSpec,
    ) -> impl Future<Output = Result<ContainerId, RuntimeError>> + Send {
        std::future::ready(Ok(spec.name.clone()))
    }
    fn inspect(
        &self,
        _id: &ContainerId,
    ) -> impl Future<Output = Result<ContainerState, RuntimeError>> + Send {
        std::future::ready(Ok(ContainerState::Gone))
    }
    fn volume_ensure(
        &self,
        _name: &VolumeName,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        std::future::ready(Ok(()))
    }
    fn stop(
        &self,
        _id: &ContainerId,
        _grace: Duration,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        std::future::ready(Ok(()))
    }
    fn remove(
        &self,
        _id: &ContainerId,
        _force: bool,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        std::future::ready(Ok(()))
    }
    fn list_hutan(&self) -> impl Future<Output = Result<Vec<ContainerRef>, RuntimeError>> + Send {
        std::future::ready(Ok(Vec::new()))
    }
}

fn poll_ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("a ready future was pending"),
    }
}

#[test]
fn exec_launches_come_from_the_runtimes_cli() {
    let runtime = Scripted::apple();
    let id = ContainerId::new("hutan-s1").unwrap();
    let cmd = ExecSpec::new(strs(&["codex", "app-server"])).unwrap();
    assert_eq!(
        runtime.exec_pty(&id, &cmd).args,
        strs(&["exec", "-it", "hutan-s1", "codex", "app-server"])
    );
    let stdio = runtime.exec_stdio(&id, &cmd);
    assert_eq!(
        stdio.args,
        strs(&["exec", "-i", "hutan-s1", "codex", "app-server"])
    );
    assert_eq!(
        stdio.program,
        std::path::PathBuf::from("/usr/local/bin/container")
    );
}

#[test]
fn the_lifecycle_methods_are_plain_futures_and_detect_is_synchronous() {
    let runtime = Scripted::apple();
    let spec = crate::common::minimal_spec();
    assert_eq!(
        poll_ready(runtime.ensure(&spec)).unwrap().as_str(),
        "hutan-s1"
    );
    assert_eq!(
        poll_ready(runtime.inspect(&spec.name)).unwrap(),
        ContainerState::Gone
    );
    assert!(poll_ready(runtime.list_hutan()).unwrap().is_empty());

    let probe = FakeProbe::mac();
    assert_eq!(runtime.detect(&probe).state, RuntimeState::Missing);
    assert_eq!(
        runtime.caps(),
        RuntimeCaps::for_kind(RuntimeKind::AppleContainer)
    );
    assert_eq!(runtime.id().as_str(), "apple-container");
}
