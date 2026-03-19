fn main() {
    embuild::espidf::sysenv::output();

    let modem = if cfg!(feature = "modem-quectel") {
        "quectel"
    } else if cfg!(feature = "modem-simcom") {
        "simcom"
    } else if cfg!(feature = "modem-wifi") {
        "wifi"
    } else {
        "unknown"
    };

    println!("cargo:rustc-env=ATHENA_MODEM={}", modem);

    let board = if cfg!(feature = "board-xiao") {
        "xiao"
    } else if cfg!(feature = "board-wroom") {
        "wroom"
    } else {
        "unknown"
    };

    println!("cargo:rustc-env=ATHENA_BOARD={}", board);
}
