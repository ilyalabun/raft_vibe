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

/// One-way partition: leader can't reach one follower, but follower can still
/// send to the leader. Tests graceful handling of asymmetric communication.
#[tokio::test]
#[ignore]
async fn test_asymmetric_partition() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap() as usize;

    let config = chaos_test_config(&cluster);

    // Pick a follower to partition from the leader
    let follower = if leader_id == 1 { 2 } else { 1 };

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.partition_one_way(leader_id, follower).await;
        sleep(Duration::from_secs(2)).await;
        cluster.heal_partition_one_way(leader_id, follower).await;
    });

    println!(
        "Asymmetric partition: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after asymmetric partition: {:?}",
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

/// Kill the leader 3 times in rapid succession, forcing repeated elections.
/// Tests cluster stability under rapid leader churn.
#[tokio::test]
#[ignore]
async fn test_leader_kill_loop() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        for i in 1..=3 {
            sleep(Duration::from_millis(500)).await;
            if let Some(killed) = cluster.kill_leader().await {
                println!("Kill loop iteration {}: killed leader {}", i, killed);
            }
            cluster.wait_for_leader(LEADER_TIMEOUT).await;
        }
    });

    println!(
        "Leader kill loop: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after leader kill loop: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 4,
        "Some ops should succeed during leader churn: {} / {}",
        result.successful_ops,
        result.total_ops
    );

    cluster.stop().await;
}

/// Kill 3 of 5 nodes (quorum lost), wait 2s, restart all 3.
/// Cluster should stall during quorum loss then recover.
#[tokio::test]
#[ignore]
async fn test_majority_crash_and_recover() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");
    let leader_id = leader.unwrap();

    let config = chaos_test_config(&cluster);

    // Pick 3 nodes to kill (include leader for maximum disruption)
    let victims: Vec<usize> = (1..=NODE_COUNT)
        .filter(|&i| i as u64 != leader_id)
        .take(2)
        .chain(std::iter::once(leader_id as usize))
        .collect();

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        for &v in &victims {
            cluster.kill_node(v).await;
        }
        println!("Killed {} nodes (quorum lost), waiting 2s...", victims.len());
        sleep(Duration::from_secs(2)).await;
        for &v in &victims {
            cluster.start_node(v).await;
        }
        println!("Restarted all crashed nodes");
        cluster.wait_for_leader(LEADER_TIMEOUT).await;
    });

    println!(
        "Majority crash+recover: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable after majority crash and recover: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 4,
        "Some ops should succeed after recovery: {} / {}",
        result.successful_ops,
        result.total_ops
    );

    cluster.stop().await;
}

/// Add 20% packet loss to 2 nodes, verify operations still succeed and are linearizable.
#[tokio::test]
#[ignore]
async fn test_packet_loss() {

    let cluster = DockerCluster::start(COMPOSE_FILE, NODE_COUNT).await;
    let leader = cluster.wait_for_leader(LEADER_TIMEOUT).await;
    assert!(leader.is_some(), "Cluster should elect a leader");

    let config = chaos_test_config(&cluster);

    let (result, _) = tokio::join!(run_test(config), async {
        sleep(Duration::from_millis(200)).await;
        cluster.add_packet_loss(1, 20).await;
        cluster.add_packet_loss(2, 20).await;
    });

    println!(
        "Packet loss: {} total, {} successful, linearizable: {}",
        result.total_ops, result.successful_ops, result.check.is_linearizable
    );

    assert!(
        result.check.is_linearizable,
        "Should be linearizable with packet loss: {:?}",
        result.check.error
    );
    assert!(
        result.successful_ops > result.total_ops / 4,
        "Some ops should succeed with packet loss: {} / {}",
        result.successful_ops,
        result.total_ops
    );

    // Cleanup
    cluster.clear_netem(1).await;
    cluster.clear_netem(2).await;
    cluster.stop().await;
}
