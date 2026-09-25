// SPDX-License-Identifier: GPL-3.0-only

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();

    tracing::info!("Starting scratchpad applet version {VERSION}");

    cosmic_ext_applet_scratchpad::run()
}
