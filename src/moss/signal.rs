use tokio::sync::broadcast;

pub(crate) type Payload = Box<str>;

pub(crate) fn channel(capacity: usize) -> (broadcast::Sender<Payload>, broadcast::Receiver<Payload>) {
    broadcast::channel(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_emitted_signal() {
        let (tx, mut rx) = channel(16);
        let _ = tx.send("hello".into());
        assert_eq!(*rx.recv().await.unwrap(), *"hello");
    }

    #[tokio::test]
    async fn no_subscriber_does_not_panic() {
        let (tx, _) = channel(16);
        let _ = tx.send("ignored".into());
    }

    #[tokio::test]
    async fn independent_subscribers_each_get_the_signal() {
        let (tx, mut rx1) = channel(16);
        let mut rx2 = tx.subscribe();
        let _ = tx.send("ping".into());
        assert_eq!(*rx1.recv().await.unwrap(), *"ping");
        assert_eq!(*rx2.recv().await.unwrap(), *"ping");
    }
}
