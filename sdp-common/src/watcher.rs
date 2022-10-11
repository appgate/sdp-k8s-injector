pub trait SimpleWatchingProtocol<P> {
    fn initialize(&self) -> Option<P>;
    fn applied(&self) -> Option<P>;
    fn deleted(&self) -> Option<P>;
}
