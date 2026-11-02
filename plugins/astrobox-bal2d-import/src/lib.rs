//! AstroBox plugin (API level 4): compiles the user's own Blue Archive memorial-lobby assets into
//! a `.bal2d` pack for their watch, sends it to the `ba_l2d` Canopus module over CanopusConnect,
//! and adds the BAL2D layer to watchfaces. It ships no game assets.

use crate::astrobox::psys_host_v4::canopusconnect::Payload;
use crate::exports::astrobox::psys_plugin_v4::{canopusconnect as plugin_connect, event, event::EventType, lifecycle};

mod assets;
mod connect;
mod face;
mod framing;
mod state;
mod ui;
mod view;
mod watch;

wit_bindgen::generate!({
    path: "wit",
    world: "psys-world-v4-canopusconnect",
    generate_all,
});

struct Bal2dPlugin;

impl event::Guest for Bal2dPlugin {
    async fn on_event(event_type: EventType, event_payload: String) -> String {
        match event_type {
            EventType::DeviceAction => {
                connect::refresh_devices().await;
                watch::hello().await;
                ui::rerender();
            }
            EventType::Timer => {
                if event_payload == watch::TRANSFER_TIMER {
                    watch::on_timer().await;
                    ui::rerender();
                } else if event_payload == watch::STATS_TIMER {
                    watch::stats().await;
                    ui::rerender();
                }
            }
            _ => {}
        }
        String::new()
    }

    async fn on_ui_event(event_id: String, _event: event::UiEvent, event_payload: String) -> String {
        ui::on_event(&event_id, &event_payload).await;
        String::new()
    }

    async fn on_ui_render(element_id: String) {
        ui::render_main_ui(&element_id);
    }

    async fn on_card_render(_card_id: String) {}
}

impl plugin_connect::Guest for Bal2dPlugin {
    async fn on_message(device_addr: String, module: String, payload: Payload) {
        if let Some(frame) = connect::binary_frame(&module, payload) {
            watch::on_frame(&device_addr, &frame).await;
            ui::rerender();
        }
    }
}

impl lifecycle::Guest for Bal2dPlugin {
    async fn on_load() {
        tracing_subscriber::fmt().with_writer(std::io::stdout).with_ansi(false).compact().init();
        connect::refresh_devices().await;
        watch::hello().await;
    }
}

export!(Bal2dPlugin);
