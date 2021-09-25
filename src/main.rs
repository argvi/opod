use std::env;

use std::fs;
use std::ffi::OsStr;
use std::path::PathBuf;
use regex::Regex;
use std::sync::mpsc::channel;
use std::time::Duration;
use notify::{Watcher, RecursiveMode, watcher, DebouncedEvent};

use std::net::{SocketAddrV4, Ipv4Addr};
use igd::{search_gateway, SearchOptions, PortMappingProtocol};

use log::{debug, error, info};

const OPOD_REQS_WATCHER_DELAY : u64 = 10; // interval between directory samplings?
const OPOD_LEASE_TIMEOUT : u32 = 3600; // mappings should be deleted by the router after this many seconds
// filename of a request should be <external_port>-<internal_ip>-<internal_port>/opod
// regex will happily take invalid ports and ips, so we'll need to check the validity when we cast
const OPOD_REQUEST_REGEX : &'static str = r"^(\d{1,5})-(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})-(\d{1,5})\.opod$";

// use the igd crate to locate an IGD and request a TCP port mapping
// returns the external address of the mapping (ip:port). external ip is retrieved from the IGD via UPNP
fn add_port_mapping(external_tcp_port : u16,
                    internal_addr : SocketAddrV4) -> Result<SocketAddrV4, String> {
    // find an IGD (hopefully there's only one)
    // search will timeout after 10 seconds
    // TODO: handle the case of multiple IGDs by target the ip of the default gateway instead of broadcast
    let gateway = match search_gateway(SearchOptions::default()) {
        Ok(gateway) => gateway,
        Err(e) => return Err(format!("Failed to find IGD: {:?}", e)),
    };

    // request the IGD's external ip address (this is returned and only logged)
    let ip = match gateway.get_external_ip() {
        Ok(ip) => ip,
        Err(e) => return Err(format!("Failed to get external ip: {:?}", e)),
    };
    debug!("Gateway external ip: {}", ip);

    // request a tcp port mapping from the igd
    match gateway.add_port(PortMappingProtocol::TCP,
                     external_tcp_port,
                     internal_addr,
                     OPOD_LEASE_TIMEOUT,
                     "OPOD forwarding") {
        Ok(_) => Ok(SocketAddrV4::new(ip, external_tcp_port)),
        Err(e) => Err(format!("Failed add port mapping: {:?}", e)),
    }
}

// called for ANY file created in the watched directory
// if the filename matches the request format, extract the external port and internal address and request the mapping
// deletes commands that were handled even if they failed (will be visible through Syncthing)
fn handle_new_file(path_buf : PathBuf) {
    // default to empty string on any failure, they will just fail the regex check
    let request = path_buf.file_name().unwrap_or(OsStr::new("")).to_str().unwrap_or("");
    debug!("newly created file name: {}", request);

    // TODO: compile the regex only once
    let re = Regex::new(OPOD_REQUEST_REGEX).unwrap();

    match re.captures(request) {
        Some(captures) => { // if the filename matched the request format
            info!("Found new request: {}", request);
            let external_port = match captures.get(1).unwrap().as_str().parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    error!("Invalid external port!");
                    return
                },
            };
            let internal_ip = match captures.get(2).unwrap().as_str().parse::<Ipv4Addr>() {
                Ok(ip) => ip,
                Err(_) => {
                    error!("Invalid internal ip!");
                    return
                },
            };
            let internal_port = match captures.get(3).unwrap().as_str().parse::<u16>() {
                Ok(port) => port,
                Err(_) => {
                    error!("Invalid internal port!");
                    return
                },
            };
            let internal_addr = SocketAddrV4::new(internal_ip, internal_port);

            match add_port_mapping(external_port, internal_addr) {
                Ok(external_addr) => info!("Added port mapping: {}:{} -> {}:{}",
                                              external_addr.ip(),
                                              external_addr.port(),
                                              internal_addr.ip(),
                                              internal_addr.port()),
                Err(e) => error!("Failed adding port mapping: {:?}", e),
            };

            // remove request file
            match fs::remove_file(path_buf.as_path()) {
                Err(e) => error!("Failed removing request file {}: {:?}", request, e),
                Ok(_) => debug!("Request {} handled and removed", request)
            };
        },
        None => {},
    };
}

fn main() -> Result<(), &'static str>{
    // basic command line parsing
    let args: Vec<String> = env::args().collect();

    let cmd_arg = match args.len() {
        2 => &args[1],
        _ => {
            eprintln!("Usage: {} <forwarding_requests_path>", &args[0]);
            return Err("Invalid usage");
        }
    };

    let reqs_path = match cmd_arg.as_str() {
        "-h" | "--help" => {
            println!("Usage: {} <forwarding_requests_path>", &args[0]);
            return Ok(());
        },
        _ => cmd_arg,
    };

    let reqs_path_metadata = match fs::metadata(reqs_path) {
        Ok(metadata) => metadata,
        Err(e) => {
            eprintln!("Failed accessing {}: {:?}", reqs_path, e);
            return Err("Invalid argument");
        }
    };

    if !reqs_path_metadata.is_dir() {
        eprintln!("{} is not a directory", reqs_path);
        return Err("Invalid argument");
    }

    // initialize watcher to monitor the specified requests directory
    let (tx, rx) = channel();
    let mut watcher = watcher(tx, Duration::from_secs(OPOD_REQS_WATCHER_DELAY)).unwrap();
    watcher.watch(reqs_path, RecursiveMode::NonRecursive).unwrap();

    env_logger::init();

    loop {
        match rx.recv() {
            Ok(event) => {
                match event {
                    DebouncedEvent::Create(path_buf) => handle_new_file(path_buf),
                    _ => {}, // ignore any event that isn't file creation
                }
            },
            Err(e) => error!("watch error: {:?}", e),
        }
    };
}
