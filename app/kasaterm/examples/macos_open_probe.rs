#[cfg(target_os = "macos")]
#[path = "../src/macos_open.rs"]
#[allow(dead_code)]
mod macos_open;

#[cfg(target_os = "macos")]
#[derive(Debug)]
enum UserEvent {
    OpenMarkdownWindow(String),
    NativeConfirm(bool),
    Timeout,
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
    use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
    use winit::window::WindowId;

    struct Probe {
        proxy: EventLoopProxy<UserEvent>,
        log: std::fs::File,
        opened: usize,
    }

    impl ApplicationHandler<UserEvent> for Probe {
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
            macos_open::install_open_doc_handler(self.proxy.clone());
            writeln!(self.log, "resumed").unwrap();
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
            match event {
                UserEvent::OpenMarkdownWindow(path) => {
                    writeln!(self.log, "open {path}").unwrap();
                    self.opened += 1;
                    if self.opened == 3 {
                        event_loop.exit();
                    }
                }
                UserEvent::Timeout => event_loop.exit(),
                UserEvent::NativeConfirm(confirmed) => unreachable!("unexpected confirmation: {confirmed}"),
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    let mut log = std::fs::File::create(std::env::var_os("KASATERM_OPEN_PROBE_LOG").ok_or("missing probe log")?)?;
    writeln!(log, "pid {}", std::process::id())?;
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    builder.with_activation_policy(ActivationPolicy::Accessory).with_activate_ignoring_other_apps(false);
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    if std::env::var_os("KASATERM_OPEN_PROBE_RESUMED_ONLY").is_none() {
        macos_open::prepare_open_doc_handler(proxy.clone());
    }
    let timeout_proxy = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(20));
        let _ = timeout_proxy.send_event(UserEvent::Timeout);
    });
    let mut probe = Probe { proxy, log, opened: 0 };
    event_loop.run_app(&mut probe)?;
    if probe.opened != 3 {
        return Err(format!("expected 3 documents, received {}", probe.opened).into());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macos_open_probe requires macOS");
    std::process::exit(2);
}
