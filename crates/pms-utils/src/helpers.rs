use owo_colors::OwoColorize;
use pms_types::Block;

pub fn print_block_full(b: &Block) {
    println!(
        "{}",
        "---------------- BLOCK ----------------".bright_black()
    );

    // ID en jaune
    println!("{} {}", "id:".bright_black(), b.id.yellow());

    // Parents en cyan
    println!("{} {:?}", "parents:".bright_black(), b.parents.cyan());

    // Nonce en rouge
    println!("{} {}", "nonce:".bright_black(), b.nonce.red());

    // Payload -> JSON pretty si possible
    match &b.payload {
        Some(p) => {
            if let Ok(js) = serde_json::to_string_pretty(p) {
                println!("{} {}", "payload:".bright_black(), js.green());
            } else {
                println!("{} {:#?}", "payload:".bright_black(), p);
            }
        }
        None => println!("{} {}", "payload:".bright_black(), "null".bright_black()),
    }
}

pub fn ts_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
