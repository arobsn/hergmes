use hergmes::telemetry::{self, default_subscriber};
use tracing::info;

fn main() {
    telemetry::init(default_subscriber());

    info!("Hello, world!");
}
