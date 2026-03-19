use dstate_e2e::scenarios::*;
use dstate_kameo::KameoRuntime;

// All scenarios use active-push config unless otherwise specified.

#[tokio::test]
async fn e2e_01_basic_mutation() {
    test_basic_mutation::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_02_bidirectional_sync() {
    test_bidirectional_sync::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_03_multi_node_fanout() {
    test_multi_node_fanout::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_04_delta_sync_roundtrip() {
    test_delta_sync_roundtrip::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_05_node_join_snapshot() {
    test_node_join_snapshot::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_06_node_leave_cleans_views() {
    test_node_leave_cleans_views::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_07_periodic_sync() {
    test_periodic_sync::<KameoRuntime>().await;
}

#[tokio::test]
async fn e2e_08_feed_lazy_pull() {
    test_feed_lazy_pull::<KameoRuntime>().await;
}

#[tokio::test]
async fn e2e_09_concurrent_mutations() {
    test_concurrent_mutations::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_10_query_freshness() {
    test_query_freshness::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_11_multiple_mutations() {
    test_multiple_mutations::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_12_cluster_events() {
    test_cluster_events::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_f01_network_partition() {
    test_network_partition::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_f02_total_network_failure() {
    test_total_network_failure::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_f03_packet_loss() {
    test_packet_loss::<KameoRuntime>().await;
}

#[tokio::test]
async fn e2e_f04_corruption() {
    test_corruption::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_f05_node_crash_restart() {
    test_node_crash_restart::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_f06_message_delay() {
    test_message_delay::<KameoRuntime>(active_push_config()).await;
}

#[tokio::test]
async fn e2e_f07_rapid_mutations_under_loss() {
    test_rapid_mutations_under_loss::<KameoRuntime>().await;
}
