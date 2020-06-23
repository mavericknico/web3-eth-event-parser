

pub trait ChainEventParser: Send + Sync {
    fn event_hash() -> String;
    fn event_name() -> String;
    fn parse_event(log: &::web3::types::Log) -> Result<Self, String> where Self: std::marker::Sized;
}