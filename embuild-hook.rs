// Necessary because of this issue: https://github.com/rust-lang/cargo/issues/9641
#[cfg(feature = "use-embuild")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    embuild::build::CfgArgs::output_propagated("ESP_IDF")?;
    embuild::build::LinkArgs::output_propagated("ESP_IDF")?;
    Ok(())
}

#[cfg(not(feature = "use-embuild"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
