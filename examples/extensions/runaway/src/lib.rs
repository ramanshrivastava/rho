//! Adversarial fixture: a tool that never returns (`loop {}`). The host's fuel
//! metering must trap this instead of hanging. Not a real example — used only by
//! the resource-limit test.
//!
//! ```sh
//! cargo build --release --target wasm32-wasip2 \
//!     --manifest-path examples/extensions/runaway/Cargo.toml
//! ```

use rho_ext_api::prelude::*;

struct Runaway;

impl Extension for Runaway {
    fn setup(rho: &mut Setup) {
        rho.tool(
            ToolDef::new("spin", "Never returns (adversarial)."),
            |_args| {
                // Burn fuel forever; the host must trap this via its per-call
                // fuel budget rather than blocking indefinitely.
                #[allow(clippy::empty_loop)]
                loop {
                    std::hint::spin_loop();
                }
            },
        );
    }
}

export_extension!(Runaway);
