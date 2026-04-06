use std::process::Command;

fn main() {
    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let save_path = args.iter().position(|a| a == "-o" || a == "--output")
        .and_then(|i| args.get(i + 1))
        .cloned();

    // Request screenshot from compositor
    let mut client = match kara_ipc::IpcClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kara-glimpse: failed to connect to compositor: {e}");
            std::process::exit(1);
        }
    };

    let response = match client.request(&kara_ipc::Request::Screenshot) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("kara-glimpse: IPC error: {e}");
            std::process::exit(1);
        }
    };

    let capture_path = match response {
        kara_ipc::Response::ScreenshotDone { path } => path,
        kara_ipc::Response::Error { message } => {
            eprintln!("kara-glimpse: compositor error: {message}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("kara-glimpse: unexpected response");
            std::process::exit(1);
        }
    };

    // Wait briefly for compositor to write the file
    for _ in 0..50 {
        if std::path::Path::new(&capture_path).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    if !std::path::Path::new(&capture_path).exists() {
        eprintln!("kara-glimpse: screenshot file not created: {capture_path}");
        std::process::exit(1);
    }

    // Copy to clipboard via wl-copy
    match Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(std::fs::File::open(&capture_path).expect("open screenshot"))
        .spawn()
    {
        Ok(mut child) => { child.wait().ok(); }
        Err(_) => {
            eprintln!("kara-glimpse: wl-copy not found, clipboard copy skipped");
        }
    }

    // If user wants to save, copy to their path
    if let Some(dest) = save_path {
        if let Err(e) = std::fs::copy(&capture_path, &dest) {
            eprintln!("kara-glimpse: failed to save to {dest}: {e}");
        } else {
            println!("{dest}");
        }
    } else {
        println!("{capture_path}");
    }
}
