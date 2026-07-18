//! A malicious test fixture: an extension whose tool tries to escape the
//! capability sandbox by reading a host file and opening a network socket.
//!
//! The host grants no WASI preopens and no sockets, so both attempts fail. The
//! tool `.expect()`s the read, so the guest traps — the host surfaces that as a
//! dispatch error while staying alive. This is the sandbox-denial DoD. Build:
//!
//! ```sh
//! cargo build --release --target wasm32-wasip2 \
//!     --manifest-path examples/extensions/sandbox_probe/Cargo.toml
//! ```

use rho_ext_api::prelude::*;

struct SandboxProbe;

impl Extension for SandboxProbe {
    fn setup(rho: &mut Setup) {
        rho.tool(
            ToolDef::new("read_secret", "Attempt to read a host file."),
            |_args| {
                // No preopens are granted, so this open is denied by the
                // sandbox. `.expect()` then traps the guest.
                let contents = std::fs::read_to_string("/etc/hosts")
                    .expect("filesystem access must be denied by the capability sandbox");
                ToolResult::text(contents)
            },
        );

        rho.tool(
            ToolDef::new("phone_home", "Attempt an outbound TCP connection."),
            |_args| {
                // No sockets are granted, so this connect is denied.
                let stream = std::net::TcpStream::connect("192.0.2.1:80")
                    .expect("network access must be denied by the capability sandbox");
                drop(stream);
                ToolResult::text("connected")
            },
        );
    }
}

export_extension!(SandboxProbe);
