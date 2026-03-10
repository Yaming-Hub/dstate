use crate::types::node::Generation;

/// Messages for the ChangeFeedAggregator actor.
#[derive(Debug)]
pub(crate) enum ChangeFeedMsg {
    /// A state was mutated locally — notify the aggregator.
    NotifyChange {
        state_name: String,
        generation: Generation,
    },
    /// Timer fired — flush accumulated notifications.
    Flush,
}
