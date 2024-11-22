fn main() -> Result<(), Box<dyn std::error::Error>> {
    simple_logger::init()?;
    let target_pid = std::env::args().nth(1).unwrap().parse::<i32>().unwrap();
    let path = tty_driver::find_tty_for_pid(target_pid);
    println!("Result: {path:?}");
    Ok(())
}
