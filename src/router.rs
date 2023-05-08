use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ipnetwork::IpNetwork;
use rand::seq::SliceRandom;
use rand::Rng;

pub struct Bgp {
    events: mpsc::Receiver<RibToBgpEvent>,
    local_rib: BgpLocalRib,
}

impl Bgp {
    pub fn new(events: mpsc::Receiver<RibToBgpEvent>) -> Self {
        Self {
            events,
            local_rib: Default::default(),
        }
    }

    pub fn run(mut self) {
        loop {
            match self.events.recv() {
                Ok(ev) => {
                    self.handle_event(ev);
                }
                Err(mpsc::RecvError) => {
                    warn!("BGP lost communication with the RIB");
                }
            }
        }
    }

    fn handle_event(&mut self, event: RibToBgpEvent) {
        match event {
            RibToBgpEvent::RedistAdd(vrf_id, prefix, next_hop) => {
                self.local_rib.add_path(vrf_id, prefix, next_hop);
            }
            RibToBgpEvent::RedistDel(vrf_id, prefix) => {
                self.local_rib.del_path(vrf_id, prefix);
            }
        }
    }
}

#[derive(Debug, Default)]
struct BgpLocalRib {
    tables: HashMap<u32, BgpLocalRibTable>,
}

impl BgpLocalRib {
    #[instrument(skip(self), fields(vrf_id = %vrf_id, prefix = %prefix, next_hop = %next_hop))]
    fn add_path(&mut self, vrf_id: u32, prefix: IpNetwork, next_hop: IpAddr) {
        let table = self
            .tables
            .entry(vrf_id)
            .or_insert_with(BgpLocalRibTable::default);
        table.add_route(prefix, next_hop);
    }

    #[instrument(skip(self), fields(vrf_id = %vrf_id, prefix = %prefix))]
    fn del_path(&mut self, vrf_id: u32, prefix: IpNetwork) {
        match self.tables.entry(vrf_id) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().del_route(prefix);
                if entry.get().is_empty() {
                    info!("Table is now empty, dropping it");
                    entry.remove();
                }
            }
            Entry::Vacant(_) => {
                warn!("No path to remove (table doesn't exist)")
            }
        }
    }
}

#[derive(Debug, Default)]
struct BgpLocalRibTable {
    paths: HashMap<IpNetwork, IpAddr>,
}

impl BgpLocalRibTable {
    #[instrument(skip_all)]
    fn add_route(&mut self, prefix: IpNetwork, next_hop: IpAddr) {
        self.paths
            .entry(prefix)
            .and_modify(|nh| {
                if nh != &next_hop {
                    info!(old_next_hop = ?nh, "Updated path's next-hop");
                    *nh = next_hop
                }
            })
            .or_insert_with(|| {
                info!("New path");
                next_hop
            });
    }

    fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    #[instrument(skip_all)]
    fn del_route(&mut self, prefix: IpNetwork) {
        match self.paths.remove(&prefix) {
            Some(next_hop) => {
                info!(next_hop = ?next_hop, "Removed path");
            }
            None => warn!("No path to remove (prefix not found in table)"),
        }
    }
}

#[derive(Debug)]
pub struct Rib {
    tx: mpsc::Sender<RibToBgpEvent>,
}

impl Rib {
    pub fn new(tx: mpsc::Sender<RibToBgpEvent>) -> Self {
        Self { tx }
    }

    pub fn run(self) {
        let prefixes = vec![
            "1.0.0.0/8".parse().unwrap(),
            "192.168.1.1/32".parse().unwrap(),
            "10.10.1.0/24".parse().unwrap(),
            "3.3.240.0/16".parse().unwrap(),
        ];
        let next_hops = vec![
            "1.1.1.1".parse().unwrap(),
            "11.22.33.44".parse().unwrap(),
            "10.10.10.10".parse().unwrap(),
        ];
        let vrf_ids = vec![0, 1, 2, 3];
        let mut routes: HashSet<(u32, IpNetwork)> = HashSet::new();
        let mut rng = rand::thread_rng();
        loop {
            thread::sleep(Duration::from_secs(1));
            let prefix = prefixes.choose(&mut rng).unwrap();
            let next_hop = next_hops.choose(&mut rng).unwrap();
            let vrf_id = vrf_ids.choose(&mut rng).unwrap();

            let route = (*vrf_id, *prefix);
            if routes.contains(&route) && rng.gen::<bool>() {
                routes.remove(&route);
                self.tx
                    .send(RibToBgpEvent::RedistDel(*vrf_id, *prefix))
                    .unwrap();
            } else {
                self.tx
                    .send(RibToBgpEvent::RedistAdd(*vrf_id, *prefix, *next_hop))
                    .unwrap();
            }
        }
    }
}

#[derive(Debug)]
pub enum RibToBgpEvent {
    RedistAdd(u32, IpNetwork, IpAddr),
    RedistDel(u32, IpNetwork),
}
