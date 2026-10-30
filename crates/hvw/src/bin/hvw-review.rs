//! `hvw-review`: the popup of the herdr plugin. It shows the pending
//! egress requests with the exact host, method and path that the
//! gatekeeper refused, next to the reason of the agent. The user allows a
//! request for the VM, with or without the Nix change for its policies, or
//! denies it.
//!
//! It is the terminal UI of `hvw`, opened on the pending requests.

use anyhow::Result;
use hvw::tui::View;

#[tokio::main]
async fn main() -> Result<()> {
    hvw::tui::run(View::Pending).await
}
