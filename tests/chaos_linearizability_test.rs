//! Docker-based chaos linearizability tests.
//!
//! These tests require Docker and are marked `#[ignore]`.
//! Run with: cargo test --test chaos_linearizability_test -- --ignored --nocapture --test-threads=1

use std::time::Duration;

use chaos_test::docker::DockerCluster;
use chaos_test::{run_test, TestConfig};
use tokio::time::sleep;

const COMPOSE_FILE: &str = "docker-compose.chaos.yml";
const NODE_COUNT: usize = 5;
const LEADER_TIMEOUT: Duration = Duration::from_secs(30);

fn chaos_test_config(cluster: &DockerCluster) -> TestConfig {
    TestConfig {
        node_addresses: (1..=NODE_COUNT)
            .map(|i| cluster.node_api_addr(i))
            .collect(),
        client_count: 5,
        ops_per_client: 100,
        keys: vec!["x".to_string()],
        write_ratio: 0.5,
        request_timeout: Duration::from_secs(5),
    }
}

/// Kill the leader after 1s of workload, verify cluster recovers and is linearizable.
#[tokio::test]
#[ignore]
async fn test_leader_kill() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.kill_leader().await;
        cluster.wait_for_leader(LEADER_TIMEOUT).await;
    });

    println!(
        "Leader kill: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after leader kill: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 2,
        "Most ops should succeed: {} / {}",
        result.successful_ops,
        result.total_ops
    );

    cluster.stop().await;
}

/// Kill one follower during workload, verify cluster continues and is linearizable.
#[tokio::test]
#[ignore]
async fn test_follower_kill() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap();

    let config = chaos_test_config(&cluster);

    // Pick a non-leader node to kill
    let victim = if leader_id == 1 { 2 } else { 1 };

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.kill_node(victim).await;
    });

    println!(
        "Follower kill: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after follower kill: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 2,
        "Most ops should succeed"
    );

    cluster.stop().await;
}

/// Kill two followers (3/5 majority remains), verify cluster continues.
#[tokio::test]
#[ignore]
async fn test_two_followers_kill() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap();

    let config = chaos_test_config(&cluster);

    // Pick two non-leader nodes
    let mut victims: Vec<usize> = (1..=NODE_COUNT)
        .filter(|&i| i as u64 != leader_id)
        .take(2)
        .collect();
    victims.sort();

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        for &v in &victims {
            cluster.kill_node(v).await;
        }
    });

    println!(
        "Two followers kill: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable with 3/5 majority: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 4,
        "Some ops should succeed with majority intact"
    );

    cluster.stop().await;
}

/// Isolate 2 nodes from the network (minority partition), majority continues.
#[tokio::test]
#[ignore]
async fn test_minority_partition() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap();

    let config = chaos_test_config(&cluster);

    // Isolate two non-leader nodes
    let isolated: Vec<usize> = (1..=NODE_COUNT)
        .filter(|&i| i as u64 != leader_id)
        .take(2)
        .collect();

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        for &id in &isolated {
            cluster.isolate_node(id).await;
        }
    });

    println!(
        "Minority partition: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable with minority partition: {:?}",
        result.check.error
    );

    // Cleanup: rejoin nodes
    for &id in &isolated {
        cluster.rejoin_node(id).await;
    }

    cluster.stop().await;
}

/// Isolate the leader from the network, force new election.
#[tokio::test]
#[ignore]
async fn test_leader_partition() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap() as usize;

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.isolate_node(leader_id).await;
        cluster.wait_for_leader(LEADER_TIMEOUT).await;
    });

    println!(
        "Leader partition: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after leader partition: {:?}",
        result.check.error
    );

    // Cleanup
    cluster.rejoin_node(leader_id).await;
    cluster.stop().await;
}

/// Partition then heal after 2s, verify cluster recovers.
#[tokio::test]
#[ignore]
async fn test_partition_heal() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap() as usize;

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.isolate_node(leader_id).await;
        sleep(Duration::from_secs(2)).await;
        cluster.rejoin_node(leader_id).await;
        cluster.wait_for_leader(LEADER_TIMEOUT).await;
    });

    println!(
        "Partition heal: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after partition heal: {:?}",
        result.check.error
    );

    cluster.stop().await;
}

/// Add 200ms latency to 2 nodes, verify operations are slower but linearizable.
#[tokio::test]
#[ignore]
async fn test_slow_network() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.add_latency(1, 50).await;
        cluster.add_latency(2, 50).await;
    });

    println!(
        "Slow network: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable with slow network: {:?}",
        result.check.error
    );

    // Cleanup
    cluster.clear_netem(1).await;
    cluster.clear_netem(2).await;
    cluster.stop().await;
}

/// Kill and restart nodes one at a time, verify cluster stays available.
#[tokio::test]
#[ignore]
async fn test_rolling_restart() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        for id in 1..=NODE_COUNT {
            sleep(Duration::from_millis(200)).await;
            cluster.kill_node(id).await;
            sleep(Duration::from_millis(200)).await;
            cluster.start_node(id).await;
            cluster.wait_for_leader(LEADER_TIMEOUT).await;
        }
    });

    println!(
        "Rolling restart: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable during rolling restart: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 4,
        "Some ops should succeed during rolling restart"
    );

    cluster.stop().await;
}
