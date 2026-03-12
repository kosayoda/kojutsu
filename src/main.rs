use color_eyre::Result;

fn main() -> Result<()> {
    jujujutsu::setup()?;

    tracing::debug!("Debug logging enabled.");
    tracing::info!("Hello jujujutsu");

    Ok(())
}
