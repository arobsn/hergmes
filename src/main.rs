use std::error::Error;

use dotenvy::dotenv;
use hergmes::telemetry::{self, default_subscriber};
use tracing::info;

fn main() -> Result<(), Box<dyn Error>> {
    dotenv()?;
    telemetry::init(default_subscriber());

    info!("Hello, world!");
    Ok(())
}
