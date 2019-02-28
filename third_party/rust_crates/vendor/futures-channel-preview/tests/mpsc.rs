#![feature(futures_api, async_await, await_macro)]

use futures::channel::{mpsc, oneshot};
use futures::executor::{block_on, block_on_stream};
use futures::future::{FutureExt, poll_fn};
use futures::stream::{Stream, StreamExt};
use futures::sink::{Sink, SinkExt};
use futures::task::Poll;
use futures_test::task::noop_waker_ref;
use pin_utils::pin_mut;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

trait AssertSend: Send {}
impl AssertSend for mpsc::Sender<i32> {}
impl AssertSend for mpsc::Receiver<i32> {}

#[test]
fn send_recv() {
    let (mut tx, rx) = mpsc::channel::<i32>(16);

    block_on(tx.send(1)).unwrap();
    drop(tx);
    let v: Vec<_> = block_on(rx.collect());
    assert_eq!(v, vec![1]);
}

#[test]
fn send_recv_no_buffer() {
    // Run on a task context
    block_on(poll_fn(move |lw| {
        let (tx, rx) = mpsc::channel::<i32>(0);
        pin_mut!(tx, rx);

        assert!(tx.as_mut().poll_flush(lw).is_ready());
        assert!(tx.as_mut().poll_ready(lw).is_ready());

        // Send first message
        assert!(tx.as_mut().start_send(1).is_ok());
        assert!(tx.as_mut().poll_ready(lw).is_pending());

        // poll_ready said Pending, so no room in buffer, therefore new sends
        // should get rejected with is_full.
        assert!(tx.as_mut().start_send(0).unwrap_err().is_full());
        assert!(tx.as_mut().poll_ready(lw).is_pending());

        // Take the value
        assert_eq!(rx.as_mut().poll_next(lw), Poll::Ready(Some(1)));
        assert!(tx.as_mut().poll_ready(lw).is_ready());

        // Send second message
        assert!(tx.as_mut().poll_ready(lw).is_ready());
        assert!(tx.as_mut().start_send(2).is_ok());
        assert!(tx.as_mut().poll_ready(lw).is_pending());

        // Take the value
        assert_eq!(rx.as_mut().poll_next(lw), Poll::Ready(Some(2)));
        assert!(tx.as_mut().poll_ready(lw).is_ready());

        Poll::Ready(())
    }));
}

#[test]
fn send_shared_recv() {
    let (mut tx1, rx) = mpsc::channel::<i32>(16);
    let mut rx = block_on_stream(rx);
    let mut tx2 = tx1.clone();

    block_on(tx1.send(1)).unwrap();
    assert_eq!(rx.next(), Some(1));

    block_on(tx2.send(2)).unwrap();
    assert_eq!(rx.next(), Some(2));
}

#[test]
fn send_recv_threads() {
    let (mut tx, rx) = mpsc::channel::<i32>(16);

    let t = thread::spawn(move|| {
        block_on(tx.send(1)).unwrap();
    });

    let v: Vec<_> = block_on(rx.take(1).collect());
    assert_eq!(v, vec![1]);

    t.join().unwrap();
}

#[test]
fn send_recv_threads_no_capacity() {
    let (mut tx, rx) = mpsc::channel::<i32>(0);
    let mut rx = block_on_stream(rx);

    let (readytx, readyrx) = mpsc::channel::<()>(2);
    let mut readyrx = block_on_stream(readyrx);
    let t = thread::spawn(move || {
        let mut readytx = readytx.sink_map_err(|_| panic!());
        let (send_res_1, send_res_2) = block_on(tx.send(1).join(readytx.send(())));
        send_res_1.unwrap();
        send_res_2.unwrap();
        block_on(tx.send(2).join(readytx.send(())));
    });

    readyrx.next();
    assert_eq!(rx.next(), Some(1));
    readyrx.next();
    drop(readyrx);
    assert_eq!(rx.next(), Some(2));
    drop(rx);

    t.join().unwrap();
}

#[test]
fn recv_close_gets_none() {
    let (mut tx, mut rx) = mpsc::channel::<i32>(10);

    // Run on a task context
    block_on(poll_fn(move |lw| {
        rx.close();

        assert_eq!(rx.poll_next_unpin(lw), Poll::Ready(None));
        match tx.poll_ready(lw) {
            Poll::Pending | Poll::Ready(Ok(_)) => panic!(),
            Poll::Ready(Err(e)) => assert!(e.is_disconnected()),
        };

        drop(&tx);

        Poll::Ready(())
    }));
}

#[test]
fn tx_close_gets_none() {
    let (_, mut rx) = mpsc::channel::<i32>(10);

    // Run on a task context
    block_on(poll_fn(move |lw| {
        assert_eq!(rx.poll_next_unpin(lw), Poll::Ready(None));
        Poll::Ready(())
    }));
}

// #[test]
// fn spawn_sends_items() {
//     let core = local_executor::Core::new();
//     let stream = unfold(0, |i| Some(ok::<_,u8>((i, i + 1))));
//     let rx = mpsc::spawn(stream, &core, 1);
//     assert_eq!(core.run(rx.take(4).collect()).unwrap(),
//                [0, 1, 2, 3]);
// }

// #[test]
// fn spawn_kill_dead_stream() {
//     use std::thread;
//     use std::time::Duration;
//     use futures::future::Either;
//     use futures::sync::oneshot;
//
//     // a stream which never returns anything (maybe a remote end isn't
//     // responding), but dropping it leads to observable side effects
//     // (like closing connections, releasing limited resources, ...)
//     #[derive(Debug)]
//     struct Dead {
//         // when dropped you should get Err(oneshot::Canceled) on the
//         // receiving end
//         done: oneshot::Sender<()>,
//     }
//     impl Stream for Dead {
//         type Item = ();
//         type Error = ();
//
//         fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
//             Ok(Poll::Pending)
//         }
//     }
//
//     // need to implement a timeout for the test, as it would hang
//     // forever right now
//     let (timeout_tx, timeout_rx) = oneshot::channel();
//     thread::spawn(move || {
//         thread::sleep(Duration::from_millis(1000));
//         let _ = timeout_tx.send(());
//     });
//
//     let core = local_executor::Core::new();
//     let (done_tx, done_rx) = oneshot::channel();
//     let stream = Dead{done: done_tx};
//     let rx = mpsc::spawn(stream, &core, 1);
//     let res = core.run(
//         Ok::<_, ()>(())
//         .into_future()
//         .then(move |_| {
//             // now drop the spawned stream: maybe some timeout exceeded,
//             // or some connection on this end was closed by the remote
//             // end.
//             drop(rx);
//             // and wait for the spawned stream to release its resources
//             done_rx
//         })
//         .select2(timeout_rx)
//     );
//     match res {
//         Err(Either::A((oneshot::Canceled, _))) => (),
//         _ => {
//             panic!("dead stream wasn't canceled");
//         },
//     }
// }

#[test]
fn stress_shared_unbounded() {
    const AMT: u32 = 10000;
    const NTHREADS: u32 = 8;
    let (tx, rx) = mpsc::unbounded::<i32>();

    let t = thread::spawn(move|| {
        let result: Vec<_> = block_on(rx.collect());
        assert_eq!(result.len(), (AMT * NTHREADS) as usize);
        for item in result {
            assert_eq!(item, 1);
        }
    });

    for _ in 0..NTHREADS {
        let tx = tx.clone();

        thread::spawn(move|| {
            for _ in 0..AMT {
                tx.unbounded_send(1).unwrap();
            }
        });
    }

    drop(tx);

    t.join().ok().unwrap();
}

#[test]
fn stress_shared_bounded_hard() {
    const AMT: u32 = 10000;
    const NTHREADS: u32 = 8;
    let (tx, rx) = mpsc::channel::<i32>(0);

    let t = thread::spawn(move|| {
        let result: Vec<_> = block_on(rx.collect());
        assert_eq!(result.len(), (AMT * NTHREADS) as usize);
        for item in result {
            assert_eq!(item, 1);
        }
    });

    for _ in 0..NTHREADS {
        let mut tx = tx.clone();

        thread::spawn(move || {
            for _ in 0..AMT {
                block_on(tx.send(1)).unwrap();
            }
        });
    }

    drop(tx);

    t.join().unwrap();
}

#[test]
fn stress_receiver_multi_task_bounded_hard() {
    const AMT: usize = 10_000;
    const NTHREADS: u32 = 2;

    let (mut tx, rx) = mpsc::channel::<usize>(0);
    let rx = Arc::new(Mutex::new(Some(rx)));
    let n = Arc::new(AtomicUsize::new(0));

    let mut th = vec![];

    for _ in 0..NTHREADS {
        let rx = rx.clone();
        let n = n.clone();

        let t = thread::spawn(move || {
            let mut i = 0;

            loop {
                i += 1;
                let mut rx_opt = rx.lock().unwrap();
                if let Some(rx) = &mut *rx_opt {
                    if i % 5 == 0 {
                        let item = block_on(rx.next());

                        if item.is_none() {
                            *rx_opt = None;
                            break;
                        }

                        n.fetch_add(1, Ordering::Relaxed);
                    } else {
                        // Just poll
                        let n = n.clone();
                        match rx.poll_next_unpin(noop_waker_ref()) {
                            Poll::Ready(Some(_)) => {
                                n.fetch_add(1, Ordering::Relaxed);
                            }
                            Poll::Ready(None) => {
                                *rx_opt = None;
                                break
                            },
                            Poll::Pending => {},
                        }
                    }
                } else {
                    break;
                }
            }
        });

        th.push(t);
    }


    for i in 0..AMT {
        block_on(tx.send(i)).unwrap();
    }
    drop(tx);

    for t in th {
        t.join().unwrap();
    }

    assert_eq!(AMT, n.load(Ordering::Relaxed));
}

/// Stress test that receiver properly receives all the messages
/// after sender dropped.
#[test]
fn stress_drop_sender() {
    fn list() -> impl Stream<Item=i32> {
        let (tx, rx) = mpsc::channel(1);
        thread::spawn(move || {
            block_on(send_one_two_three(tx));
        });
        rx
    }

    for _ in 0..10000 {
        let v: Vec<_> = block_on(list().collect());
        assert_eq!(v, vec![1, 2, 3]);
    }
}

async fn send_one_two_three(mut tx: mpsc::Sender<i32>) {
    for i in 1..=3 {
        await!(tx.send(i)).unwrap();
    }
}

/// Stress test that after receiver dropped,
/// no messages are lost.
fn stress_close_receiver_iter() {
    let (tx, rx) = mpsc::unbounded();
    let mut rx = block_on_stream(rx);
    let (unwritten_tx, unwritten_rx) = std::sync::mpsc::channel();
    let th = thread::spawn(move || {
        for i in 1.. {
            if let Err(_) = tx.unbounded_send(i) {
                unwritten_tx.send(i).expect("unwritten_tx");
                return;
            }
        }
    });

    // Read one message to make sure thread effectively started
    assert_eq!(Some(1), rx.next());

    rx.close();

    for i in 2.. {
        match rx.next() {
            Some(r) => assert!(i == r),
            None => {
                let unwritten = unwritten_rx.recv().expect("unwritten_rx");
                assert_eq!(unwritten, i);
                th.join().unwrap();
                return;
            }
        }
    }
}

#[test]
fn stress_close_receiver() {
    for _ in 0..10000 {
        stress_close_receiver_iter();
    }
}

async fn stress_poll_ready_sender(mut sender: mpsc::Sender<u32>, count: u32) {
    for i in (1..=count).rev() {
        await!(sender.send(i)).unwrap();
    }
}

/// Tests that after `poll_ready` indicates capacity a channel can always send without waiting.
#[test]
fn stress_poll_ready() {
    const AMT: u32 = 1000;
    const NTHREADS: u32 = 8;

    /// Run a stress test using the specified channel capacity.
    fn stress(capacity: usize) {
        let (tx, rx) = mpsc::channel(capacity);
        let mut threads = Vec::new();
        for _ in 0..NTHREADS {
            let sender = tx.clone();
            threads.push(thread::spawn(move || {
                block_on(stress_poll_ready_sender(sender, AMT))
            }));
        }
        drop(tx);

        let result: Vec<_> = block_on(rx.collect());
        assert_eq!(result.len() as u32, AMT * NTHREADS);

        for thread in threads {
            thread.join().unwrap();
        }
    }

    stress(0);
    stress(1);
    stress(8);
    stress(16);
}

#[test]
fn try_send_1() {
    const N: usize = 3000;
    let (mut tx, rx) = mpsc::channel(0);

    let t = thread::spawn(move || {
        for i in 0..N {
            loop {
                if tx.try_send(i).is_ok() {
                    break
                }
            }
        }
    });

    let result: Vec<_> = block_on(rx.collect());
    for (i, j) in result.into_iter().enumerate() {
        assert_eq!(i, j);
    }

    t.join().unwrap();
}

#[test]
fn try_send_2() {
    let (mut tx, rx) = mpsc::channel(0);
    let mut rx = block_on_stream(rx);

    tx.try_send("hello").unwrap();

    let (readytx, readyrx) = oneshot::channel::<()>();

    let th = thread::spawn(move || {
        block_on(poll_fn(|lw| {
            assert!(tx.poll_ready(lw).is_pending());
            Poll::Ready(())
        }));

        drop(readytx);
        block_on(tx.send("goodbye")).unwrap();
    });

    drop(block_on(readyrx));
    assert_eq!(rx.next(), Some("hello"));
    assert_eq!(rx.next(), Some("goodbye"));
    assert_eq!(rx.next(), None);

    th.join().unwrap();
}

#[test]
fn try_send_fail() {
    let (mut tx, rx) = mpsc::channel(0);
    let mut rx = block_on_stream(rx);

    tx.try_send("hello").unwrap();

    // This should fail
    assert!(tx.try_send("fail").is_err());

    assert_eq!(rx.next(), Some("hello"));

    tx.try_send("goodbye").unwrap();
    drop(tx);

    assert_eq!(rx.next(), Some("goodbye"));
    assert_eq!(rx.next(), None);
}

#[test]
fn try_send_recv() {
    let (mut tx, mut rx) = mpsc::channel(1);
    tx.try_send("hello").unwrap();
    tx.try_send("hello").unwrap();
    tx.try_send("hello").unwrap_err(); // should be full
    rx.try_next().unwrap();
    rx.try_next().unwrap();
    rx.try_next().unwrap_err(); // should be empty
    tx.try_send("hello").unwrap();
    rx.try_next().unwrap();
    rx.try_next().unwrap_err(); // should be empty
}
