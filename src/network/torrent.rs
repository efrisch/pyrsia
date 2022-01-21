/*
   Copyright 2021 JFrog Ltd

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

extern crate base64;
extern crate reqwest;
extern crate serde;
extern crate serde_json;
extern crate synapse_rpc as rpc;
extern crate tungstenite as ws;
extern crate url;

use error_chain::bail;
use std::process;

use super::error::{ErrorKind, Result, ResultExt};
use log::{error, info};

use rpc::resource::{Resource, ResourceKind, SResourceUpdate};
use synapse_rpc::message::{self, CMessage, SMessage};

use super::client::Client;
use prettytable::Table;
use prettytable::{cell, row};
use url::Url;

pub async fn add_torrent(
    server: &str,
    pass: &str,
    directory: Option<&str>,
    files: Vec<&str>,
) -> Result<()> {
    let mut url = match Url::parse(server) {
        Ok(url) => url,
        Err(e) => {
            error!("Server URL {} is not valid: {}", server, e);
            process::exit(1);
        }
    };
    url.query_pairs_mut().append_pair("password", pass);

    let client = match Client::new(url.clone()) {
        Ok(c) => c,
        Err(_) => {
            error!("Failed to connect to synapse, ensure your URI and password are correct");
            process::exit(1);
        }
    };
    add(
        client,
        url.as_str(),
        files,
        directory,
        true,  // paused
        false, // imported
    )
}

fn add(
    mut c: Client,
    _url: &str,
    files: Vec<&str>,
    dir: Option<&str>,
    start: bool,
    _import: bool,
) -> Result<()> {
    for file in files {
        if let Ok(magnet) = Url::parse(file) {
            add_magnet(&mut c, magnet, dir, start)?;
        }
    }
    Ok(())
}

fn add_magnet(c: &mut Client, magnet: Url, dir: Option<&str>, start: bool) -> Result<()> {
    let msg = CMessage::UploadMagnet {
        serial: c.next_serial(),
        uri: magnet.as_str().to_owned(),
        path: dir.as_ref().map(|d| d.to_string()),
        start,
    };
    match c.rr(msg)? {
        SMessage::ResourcesExtant { ids, .. } => {
            get_(c, ids[0].as_ref(), "text")?;
        }
        SMessage::InvalidRequest(message::Error { reason, .. }) => {
            bail!("{}", reason);
        }
        _ => {
            bail!("Failed to receieve upload acknowledgement from synapse");
        }
    }
    Ok(())
}

pub fn get_(c: &mut Client, id: &str, output: &str) -> Result<()> {
    let res = get_resources(c, vec![id.to_owned()])?;
    if res.is_empty() {
        bail!("Resource not found");
    }
    match output {
        "text" => {
            info!("{}", res[0]);
        }
        "json" => {
            info!(
                "{}",
                serde_json::to_string_pretty(&res[0]).chain_err(|| ErrorKind::Serialization)?
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn get_resources(c: &mut Client, ids: Vec<String>) -> Result<Vec<Resource>> {
    let msg = CMessage::Subscribe {
        serial: c.next_serial(),
        ids: ids.clone(),
    };
    let unsub = CMessage::Unsubscribe {
        serial: c.next_serial(),
        ids,
    };

    let resources = if let SMessage::UpdateResources { resources, .. } = c.rr(msg)? {
        resources
    } else {
        bail!("Failed to received torrent resource list!");
    };

    c.send(unsub)?;

    let mut results = Vec::new();
    for r in resources {
        if let SResourceUpdate::Resource(res) = r {
            results.push(res.into_owned());
        } else {
            bail!("Failed to received full resource!");
        }
    }
    Ok(results)
}

pub fn list_cmd(server: &str, pass: &str, kind_opt: Option<&str>) -> Result<()> {
    let mut url = match Url::parse(server) {
        Ok(url) => url,
        Err(e) => {
            error!("Server URL {} is not valid: {}", server, e);
            process::exit(1);
        }
    };
    url.query_pairs_mut().append_pair("password", pass);

    let client = match Client::new(url.clone()) {
        Ok(c) => c,
        Err(_) => {
            error!("Failed to connect to synapse, ensure your URI and password are correct");
            process::exit(1);
        }
    };
    let crit = vec![];
    let kind = kind_opt.unwrap_or_else(|| "torrent");
    let output = "json";
    list(client, kind, crit, output)
}

pub fn status_cmd(server: &str, pass: &str) -> Result<()> {
    let mut url = match Url::parse(server) {
        Ok(url) => url,
        Err(e) => {
            error!("Server URL {} is not valid: {}", server, e);
            process::exit(1);
        }
    };
    url.query_pairs_mut().append_pair("password", pass);

    let client = match Client::new(url.clone()) {
        Ok(c) => c,
        Err(_) => {
            error!("Failed to connect to synapse, ensure your URI and password are correct");
            process::exit(1);
        }
    };
    status(client)
}

pub fn status(mut c: Client) -> Result<()> {
    match search(&mut c, ResourceKind::Server, vec![])?.pop() {
        Some(Resource::Server(s)) => {
            let vi = s.id.find("-").unwrap();
            let version = &s.id[..vi];
            info!(
                "synapse v{}, RPC v{}.{}",
                version,
                c.version().major,
                c.version().minor
            );
            info!(
                "UL: {}/s, DL: {}/s, total UL: {}, total DL: {}",
                fmt_bytes(s.rate_up as f64),
                fmt_bytes(s.rate_down as f64),
                fmt_bytes(s.transferred_up as f64),
                fmt_bytes(s.transferred_down as f64),
            );
        }
        _ => {
            bail!("synapse server incorrectly reported server status!");
        }
    };
    Ok(())
}

fn fmt_bytes(num: f64) -> String {
    let num = num.abs();
    let units = ["B", "kiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"];
    if num < 1_f64 {
        return format!("{} {}", num, "B");
    }
    let delimiter = 1024_f64;
    let exponent = std::cmp::min(
        (num.ln() / delimiter.ln()).floor() as i32,
        (units.len() - 1) as i32,
    );
    let pretty_bytes = format!("{:.2}", num / delimiter.powi(exponent))
        .parse::<f64>()
        .unwrap()
        * 1_f64;
    let unit = units[exponent as usize];
    format!("{} {}", pretty_bytes, unit)
}

fn search(
    c: &mut Client,
    kind: ResourceKind,
    criteria: Vec<rpc::criterion::Criterion>,
) -> Result<Vec<Resource>> {
    let s = c.next_serial();
    let msg = CMessage::FilterSubscribe {
        serial: s,
        kind,
        criteria,
    };
    if let SMessage::ResourcesExtant { ids, .. } = c.rr(msg)? {
        let ns = c.next_serial();
        c.send(CMessage::FilterUnsubscribe {
            serial: ns,
            filter_serial: s,
        })?;
        get_resources(c, ids.iter().map(std::borrow::Cow::to_string).collect())
    } else {
        bail!("Failed to receive extant resource list!");
    }
}

pub fn list(
    mut c: Client,
    kind: &str,
    crit: Vec<rpc::criterion::Criterion>,
    output: &str,
) -> Result<()> {
    let k = match kind {
        "torrent" => ResourceKind::Torrent,
        "tracker" => ResourceKind::Tracker,
        "peer" => ResourceKind::Peer,
        "piece" => ResourceKind::Piece,
        "file" => ResourceKind::File,
        "server" => ResourceKind::Server,
        _ => bail!("Unexpected resource kind {}", kind),
    };
    let results = search(&mut c, k, crit)?;
    if output == "text" {
        let mut table = Table::new();
        match k {
            ResourceKind::Torrent => {
                table.add_row(row!["Name", "Done", "DL", "UL", "DL RT", "UL RT", "Peers"]);
            }
            ResourceKind::Tracker => {
                table.add_row(row!["URL", "Torrent", "Error"]);
            }
            ResourceKind::Peer => {
                table.add_row(row!["IP", "Torrent", "DL RT", "UL RT"]);
            }
            ResourceKind::Piece => {
                table.add_row(row!["Torrent", "DLd", "Avail"]);
            }
            ResourceKind::File => {
                table.add_row(row!["Path", "Torrent", "Done", "Prio", "Avail"]);
            }
            ResourceKind::Server => {
                table.add_row(row!["DL RT", "UL RT"]);
            }
        }

        #[cfg_attr(rustfmt, rustfmt_skip)]
        for res in results {
            match k {
                ResourceKind::Torrent => {
                    let t = res.as_torrent();
                    table.add_row(row![
                                  t.name.as_ref().map(|s| s.as_str()).unwrap_or("[Unknown Magnet]"),
                                  format!("{:.2}%", t.progress * 100.),
                                  fmt_bytes(t.transferred_down as f64),
                                  fmt_bytes(t.transferred_up as f64),
                                  fmt_bytes(t.rate_down as f64) + "/s",
                                  fmt_bytes(t.rate_up as f64) + "/s",
                                  t.peers
                    ]);
                }
                ResourceKind::Tracker => {
                    let t = res.as_tracker();
                    table.add_row(row![
                                  t.url.as_str(),
                                  t.torrent_id,
                                  t.error.as_ref().map(|s| s.as_str()).unwrap_or("")
                    ]);
                }
                ResourceKind::Peer => {
                    let p = res.as_peer();
                    let rd = fmt_bytes(p.rate_down as f64) + "/s";
                    let ru = fmt_bytes(p.rate_up as f64) + "/s";
                    table.add_row(row![p.ip, p.torrent_id, rd, ru]);
                }
                ResourceKind::Piece => {
                    let p = res.as_piece();
                    table.add_row(row![p.torrent_id, p.downloaded, p.available]);
                }
                ResourceKind::File => {
                    let f = res.as_file();
                    table.add_row(row![
                                  f.path,
                                  f.torrent_id,
                                  format!("{:.2}%", f.progress as f64 * 100.),
                                  f.priority,
                                  format!("{:.2}%", f.availability as f64 * 100.)
                    ]);
                }
                ResourceKind::Server => {
                    let s = res.as_server();
                    let rd = fmt_bytes(s.rate_down as f64) + "/s";
                    let ru = fmt_bytes(s.rate_up as f64) + "/s";
                    table.add_row(row![rd, ru]);
                }
            }
        }
        table.printstd();
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).chain_err(|| ErrorKind::Serialization)?
        );
    }
    Ok(())
}
