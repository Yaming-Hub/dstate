/// Messages for the ChangeFeedAggregator actor.
#[derive(Debug)]
pub(crate) enum ChangeFeedMsg {
    /// A state was mutated locally — notify the aggregator.
    NotifyChange {
        state_name: String,
        incarnation: u64,
        age: u64,
    },
    /// Timer fired — flush accumulated notifications.
    Flush,
}
