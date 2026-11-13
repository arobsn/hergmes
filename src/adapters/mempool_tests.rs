//! The mempool poller against `SimChain` (what changes trigger a fetch, the tip check around
//! it) and against recorded `/info` responses served by `wiremock` (what the simulator can't
//! do: a clock going backwards, a tip change alone, no tip, a failed fetch).

use super::*;
use crate::sim::{
    Chain, Endpoint, SimChain,
    testing::{new_chain, ready},
};

fn poller(sim: &SimChain) -> MempoolPoller<SimChain> {
    MempoolPoller::new(sim.clone(), Config::default())
}

/// One poll, with the `/info` read the follower would pass it.
fn poll(poller: &mut MempoolPoller<SimChain>, now: Instant) -> Result<Poll, NodeError> {
    let info = ready(poller.source.info()).unwrap();
    ready(poller.poll(info, now))
}

fn snapshot(poll: Poll) -> (Arc<[UnconfirmedTransaction]>, BlockRef) {
    match poll {
        Poll::Snapshot { txs, observed_tip } => (txs, observed_tip),
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

/// A chain with a few txs in its pool.
fn busy_chain() -> Chain {
    let mut chain = new_chain();
    for seed in 1..=3 {
        chain.mempool_add(seed).expect("a pending tx");
    }
    chain
}

/// Asserts the poll fetched the pool once and read `/info` around it.
fn assert_one_fetch(sim: &SimChain, from: usize) {
    use Endpoint::{Info, Mempool};
    assert_eq!(sim.calls()[from..], [Info, Mempool, Info]);
}

mod sim {
    use super::*;

    #[test]
    fn the_first_poll_takes_the_pool_at_the_tip() {
        let sim = SimChain::new(busy_chain());
        let (txs, tip) = snapshot(poll(&mut poller(&sim), Instant::now()).unwrap());
        let chain = sim.lock();
        assert_eq!(tip, chain.tip());
        assert_eq!(&*txs, chain.mempool().as_slice());
        assert!(!txs.is_empty());
        drop(chain);
        assert_one_fetch(&sim, 0);
    }

    #[test]
    fn an_unchanged_node_is_not_fetched_again() {
        let sim = SimChain::new(busy_chain());
        let mut poller = poller(&sim);
        let now = Instant::now();
        snapshot(poll(&mut poller, now).unwrap());
        let poll = poll(&mut poller, now + Duration::from_secs(1)).unwrap();
        assert_eq!(poll, Poll::Unchanged);
        assert_eq!(sim.calls()[3..], [Endpoint::Info]);
    }

    #[test]
    fn a_pool_change_is_fetched() {
        let sim = SimChain::new(busy_chain());
        let mut poller = poller(&sim);
        let now = Instant::now();
        let (before, tip) = snapshot(poll(&mut poller, now).unwrap());
        let added = sim.lock().mempool_add(9).expect("a pending tx");
        let (after, same_tip) = snapshot(poll(&mut poller, now).unwrap());
        assert_eq!(same_tip, tip);
        assert_eq!(after.len(), before.len() + 1);
        assert!(after.iter().any(|tx| tx.id == added.id));
        assert_eq!(&*after, sim.lock().mempool().as_slice());
    }

    #[test]
    fn a_new_block_is_fetched_at_its_tip() {
        let sim = SimChain::new(busy_chain());
        let mut poller = poller(&sim);
        let now = Instant::now();
        snapshot(poll(&mut poller, now).unwrap());
        let block = sim.lock().mine(2, 1, 5);
        let (txs, tip) = snapshot(poll(&mut poller, now).unwrap());
        assert_eq!(tip, block);
        assert_eq!(&*txs, sim.lock().mempool().as_slice());
    }

    #[test]
    fn a_node_restart_gives_an_empty_snapshot() {
        let sim = SimChain::new(busy_chain());
        let mut poller = poller(&sim);
        let now = Instant::now();
        let (before, _) = snapshot(poll(&mut poller, now).unwrap());
        assert!(!before.is_empty());
        sim.lock().restart_node();
        let (after, _) = snapshot(poll(&mut poller, now).unwrap());
        assert!(after.is_empty());
    }

    #[test]
    fn the_refresh_period_refetches_an_unchanged_pool() {
        let sim = SimChain::new(busy_chain());
        let mut poller = poller(&sim);
        let now = Instant::now();
        let refresh = Config::default().refresh;
        let (first, _) = snapshot(poll(&mut poller, now).unwrap());
        let early = now + refresh - Duration::from_millis(1);
        assert_eq!(poll(&mut poller, early).unwrap(), Poll::Unchanged);
        let (second, _) = snapshot(poll(&mut poller, now + refresh).unwrap());
        assert_eq!(second, first);
        assert_one_fetch(&sim, 4);
        // The refresh period counts from the refetch.
        assert_eq!(
            poll(&mut poller, now + refresh + early.duration_since(now)).unwrap(),
            Poll::Unchanged
        );
    }

    #[test]
    fn a_block_during_the_fetch_discards_the_snapshot() {
        let sim = SimChain::new(busy_chain());
        let old_tip = sim.lock().tip();
        let mut mined = None;
        sim.on_call(move |endpoint, chain| {
            if endpoint == Endpoint::Mempool && mined.is_none() {
                mined = Some(chain.mine(2, 1, 5));
            }
        });
        let (txs, tip) = snapshot(poll(&mut poller(&sim), Instant::now()).unwrap());
        assert_ne!(tip, old_tip);
        assert_eq!(tip, sim.lock().tip());
        assert_eq!(&*txs, sim.lock().mempool().as_slice());
        assert_one_fetch(&sim, 2);
        assert_eq!(sim.calls().len(), 5);
    }

    #[test]
    fn a_same_height_reorg_during_the_fetch_discards_the_snapshot() {
        let mut chain = busy_chain();
        chain.mine(1, 1, 3);
        let sim = SimChain::new(chain);
        let old_tip = sim.lock().tip();
        let mut reorged = false;
        sim.on_call(move |endpoint, chain| {
            if endpoint == Endpoint::Mempool && !reorged {
                reorged = true;
                assert_eq!(chain.orphan(1), 1);
                chain.mine(0, 2, 11);
            }
        });
        let (_, tip) = snapshot(poll(&mut poller(&sim), Instant::now()).unwrap());
        assert_eq!(tip.height, old_tip.height);
        assert_ne!(tip.header_id, old_tip.header_id);
        assert_eq!(tip, sim.lock().tip());
        assert_eq!(sim.calls().len(), 5);
    }

    #[test]
    fn a_tip_moving_on_every_fetch_gives_up_until_the_next_poll() {
        let sim = SimChain::new(busy_chain());
        let mut seed = 0;
        sim.on_call(move |endpoint, chain| {
            if endpoint == Endpoint::Mempool {
                seed += 1;
                chain.mine(1, 1, seed);
            }
        });
        let mut poller = poller(&sim);
        let now = Instant::now();
        assert_eq!(poll(&mut poller, now).unwrap(), Poll::TipMoved);
        let fetches = sim
            .calls()
            .iter()
            .filter(|e| **e == Endpoint::Mempool)
            .count();
        assert_eq!(fetches, ATTEMPTS as usize);

        // Nothing was remembered: once the tip holds still, the same instant fetches again.
        sim.on_call(|_, _| {});
        let (_, tip) = snapshot(poll(&mut poller, now).unwrap());
        assert_eq!(tip, sim.lock().tip());
    }
}

mod node {
    use serde_json::Value;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;
    use crate::{
        clients::NodeClient,
        fixtures::{self, load, status},
        types::address::Network,
    };

    const OTHER_HEADER: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// The recorded `/info`, with the mempool time and the best full block replaced.
    fn info(update: u64, tip: Option<(&str, u32)>) -> ResponseTemplate {
        let mut body: Value = load("info");
        body["lastMemPoolUpdateTime"] = update.into();
        match tip {
            Some((header, height)) => {
                body["bestFullHeaderId"] = header.into();
                body["fullHeight"] = height.into();
            }
            None => {
                body["bestFullHeaderId"] = Value::Null;
                body["fullHeight"] = Value::Null;
            }
        }
        ResponseTemplate::new(200).set_body_json(body)
    }

    /// The recorded tip.
    fn recorded_tip() -> (String, u32) {
        let body: Value = load("info");
        let header = body["bestFullHeaderId"].as_str().unwrap().to_string();
        let height = body["fullHeight"].as_u64().unwrap() as u32;
        (header, height)
    }

    fn recorded_mempool() -> ResponseTemplate {
        ResponseTemplate::new(status("mempool"))
            .set_body_raw(fixtures::text("mempool"), "application/json")
    }

    /// Serves `infos` in order, one per `/info` read, and the recorded mempool.
    async fn node(infos: Vec<ResponseTemplate>) -> (MockServer, MempoolPoller<NodeClient>) {
        let server = MockServer::start().await;
        for info in infos {
            Mock::given(method("GET"))
                .and(path("/info"))
                .respond_with(info)
                .up_to_n_times(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/transactions/unconfirmed"))
            .respond_with(recorded_mempool())
            .mount(&server)
            .await;
        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        (server, MempoolPoller::new(client, Config::default()))
    }

    async fn poll_node(
        poller: &mut MempoolPoller<NodeClient>,
        now: Instant,
    ) -> Result<Poll, NodeError> {
        let info = poller.source.info().await?;
        poller.poll(info, now).await
    }

    async fn fetches(server: &MockServer) -> usize {
        let requests = server.received_requests().await.unwrap();
        requests
            .iter()
            .filter(|r| r.url.path() == "/transactions/unconfirmed")
            .count()
    }

    #[tokio::test]
    async fn a_snapshot_is_stamped_with_the_recorded_tip() {
        let (header, height) = recorded_tip();
        let tip = Some((header.as_str(), height));
        let (_server, mut poller) = node(vec![info(5, tip), info(5, tip)]).await;
        let (txs, observed) = snapshot(poll_node(&mut poller, Instant::now()).await.unwrap());
        assert_eq!(observed.height, height);
        assert_eq!(observed.header_id.to_string(), header);
        assert!(!txs.is_empty());
    }

    #[tokio::test]
    async fn a_clock_going_backwards_is_a_change() {
        let (header, height) = recorded_tip();
        let tip = Some((header.as_str(), height));
        let (server, mut poller) =
            node(vec![info(200, tip), info(200, tip), info(100, tip), info(100, tip)]).await;
        let now = Instant::now();
        snapshot(poll_node(&mut poller, now).await.unwrap());
        snapshot(poll_node(&mut poller, now).await.unwrap());
        assert_eq!(fetches(&server).await, 2);
    }

    #[tokio::test]
    async fn a_tip_change_alone_is_a_change() {
        let (header, height) = recorded_tip();
        let tip = Some((header.as_str(), height));
        let next = Some((OTHER_HEADER, height + 1));
        let (_server, mut poller) =
            node(vec![info(5, tip), info(5, tip), info(5, next), info(5, next)]).await;
        let now = Instant::now();
        snapshot(poll_node(&mut poller, now).await.unwrap());
        let (_, observed) = snapshot(poll_node(&mut poller, now).await.unwrap());
        assert_eq!(observed.height, height + 1);
        assert_eq!(observed.header_id.to_string(), OTHER_HEADER);
    }

    #[tokio::test]
    async fn a_node_with_no_full_block_is_not_fetched() {
        let (server, mut poller) = node(vec![info(5, None)]).await;
        assert_eq!(poll_node(&mut poller, Instant::now()).await.unwrap(), Poll::NoTip);
        assert_eq!(fetches(&server).await, 0);
    }

    #[tokio::test]
    async fn a_time_change_during_the_fetch_is_fetched_next_poll() {
        // The time moves between the two reads around the fetch: the poller remembers the
        // first, so the next poll sees the second as a change.
        let (header, height) = recorded_tip();
        let tip = Some((header.as_str(), height));
        let (server, mut poller) =
            node(vec![info(5, tip), info(6, tip), info(6, tip), info(6, tip)]).await;
        let now = Instant::now();
        snapshot(poll_node(&mut poller, now).await.unwrap());
        snapshot(poll_node(&mut poller, now).await.unwrap());
        assert_eq!(fetches(&server).await, 2);
    }

    #[tokio::test]
    async fn a_failed_fetch_is_retried_next_poll() {
        let (header, height) = recorded_tip();
        let tip = Some((header.as_str(), height));
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/info"))
            .respond_with(info(5, tip))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/transactions/unconfirmed"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/transactions/unconfirmed"))
            .respond_with(recorded_mempool())
            .mount(&server)
            .await;
        let client = NodeClient::new(reqwest::Client::new(), &server.uri(), Network::Mainnet);
        let mut poller = MempoolPoller::new(client, Config::default());
        let now = Instant::now();
        assert!(poll_node(&mut poller, now).await.is_err());
        snapshot(poll_node(&mut poller, now).await.unwrap());
        assert_eq!(poll_node(&mut poller, now).await.unwrap(), Poll::Unchanged);
    }
}
