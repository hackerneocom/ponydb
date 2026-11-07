fn main() -> Result<(), Box<dyn std::error::Error>> {
    let d = engine_sqlite::diagnostics()?;
    println!("SQLite {}\nsource_id: {}", d.version, d.source_id);
    for option in d.compile_options {
        println!("{option}");
    }
    Ok(())
}
