use colored::Colorize;
use local_ip_address::list_afinet_netifas;
use std::net::IpAddr;

/// Retrieves non-loopback local network IP addresses
pub fn get_available_local_ips() -> Vec<(String, IpAddr)> {
    let mut results = Vec::new();

    if let Ok(interfaces) = list_afinet_netifas() {
        for (name, ip) in interfaces {
            if !ip.is_loopback() {
                results.push((name, ip));
            }
        }
    }

    results
}

/// Print formatted connection guide for the peer
pub fn print_sender_connection_info(port: u16, token: &Option<String>) {
    println!(
        "\n{}",
        "================ [ HEAVYDROP P2P TRANSFER ] ================"
            .bold()
            .cyan()
    );
    println!(
        "{}",
        "Waiting for receiver peer to connect...".yellow().bold()
    );
    println!("Share one of the following commands with the receiver:\n");

    let local_ips = get_available_local_ips();
    let token_arg = match token {
        Some(t) => format!(" --token {}", t),
        None => String::new(),
    };

    if local_ips.is_empty() {
        println!(
            "  Local / Direct: {}",
            format!("heavydrop recv 127.0.0.1:{}{}", port, token_arg)
                .green()
                .bold()
        );
    } else {
        for (iface, ip) in &local_ips {
            let label = if iface.to_lowercase().contains("tailscale")
                || ip.to_string().starts_with("100.")
            {
                "Tailscale / VPN"
            } else if iface.to_lowercase().contains("wlan")
                || iface.to_lowercase().contains("wi-fi")
            {
                "Wi-Fi LAN"
            } else if iface.to_lowercase().contains("eth")
                || iface.to_lowercase().contains("ethernet")
            {
                "Ethernet LAN"
            } else {
                "Network"
            };

            println!(
                "  ▶ [{}] ({}): {}",
                label.cyan(),
                iface,
                format!("heavydrop recv {}:{}{}", ip, port, token_arg)
                    .green()
                    .bold()
            );
        }
    }

    println!(
        "\n{}",
        "Tips for Internet transfers (over WAN / across routers):".dimmed()
    );
    println!(
        "{}",
        "  1. Use Tailscale, ZeroTier, or WireGuard for automatic P2P NAT traversal.".dimmed()
    );
    println!(
        "{}",
        format!(
            "  2. Or forward TCP port {} on your router to your local IP.",
            port
        )
        .dimmed()
    );
    println!(
        "{}\n",
        "============================================================"
            .bold()
            .cyan()
    );
}
