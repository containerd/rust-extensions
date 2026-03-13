/*
   Copyright The containerd Authors.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

use std::{collections::HashMap, sync::Mutex};

use lazy_static::lazy_static;
use log::error;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::{
    error::Result,
    monitor::{ExitEvent, Subject, Topic},
};

lazy_static! {
    pub static ref MONITOR: Mutex<Monitor> = {
        let monitor = Monitor {
            seq_id: 0,
            subscribers: HashMap::new(),
            topic_subs: HashMap::new(),
        };
        Mutex::new(monitor)
    };
}

pub async fn monitor_subscribe(topic: Topic) -> Result<Subscription> {
    let mut monitor = MONITOR.lock().unwrap();
    let s = monitor.subscribe(topic)?;
    Ok(s)
}

pub async fn monitor_unsubscribe(sub_id: i64) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    monitor.unsubscribe(sub_id)
}

pub async fn monitor_notify_by_pid(pid: i32, exit_code: i32) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    let subject = Subject::Pid(pid);
    monitor.notify_topic(&Topic::Pid, &subject, exit_code);
    monitor.notify_topic(&Topic::All, &subject, exit_code);
    Ok(())
}

pub fn monitor_notify_by_pid_blocking(pid: i32, exit_code: i32) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    let subject = Subject::Pid(pid);
    monitor.notify_topic(&Topic::Pid, &subject, exit_code);
    monitor.notify_topic(&Topic::All, &subject, exit_code);
    Ok(())
}

pub async fn monitor_notify_by_exec(id: &str, exec_id: &str, exit_code: i32) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    let subject = Subject::Exec(id.into(), exec_id.into());
    monitor.notify_topic(&Topic::Exec, &subject, exit_code);
    monitor.notify_topic(&Topic::All, &subject, exit_code);
    Ok(())
}

pub struct Monitor {
    pub(crate) seq_id: i64,
    pub(crate) subscribers: HashMap<i64, Subscriber>,
    pub(crate) topic_subs: HashMap<Topic, Vec<i64>>,
}

pub(crate) struct Subscriber {
    pub(crate) topic: Topic,
    pub(crate) tx: UnboundedSender<ExitEvent>,
}

pub struct Subscription {
    pub id: i64,
    pub rx: UnboundedReceiver<ExitEvent>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let id = self.id;
        // std::sync::Mutex::lock is safe in any context (sync/async).
        // It will only block the thread for a very short time.
        if let Ok(mut monitor) = MONITOR.lock() {
            let _ = monitor.unsubscribe(id);
        }
    }
}

impl Monitor {
    pub fn subscribe(&mut self, topic: Topic) -> Result<Subscription> {
        let (tx, rx) = unbounded_channel::<ExitEvent>();
        let id = self.seq_id;
        self.seq_id += 1;
        let subscriber = Subscriber {
            tx,
            topic: topic.clone(),
        };

        self.subscribers.insert(id, subscriber);
        self.topic_subs.entry(topic).or_default().push(id);
        Ok(Subscription { id, rx })
    }

    fn notify_topic(&mut self, topic: &Topic, subject: &Subject, exit_code: i32) {
        let mut dead_subscribers = Vec::new();
        if let Some(subs) = self.topic_subs.get(topic) {
            for i in subs {
                if let Some(sub) = self.subscribers.get(i) {
                    if let Err(e) = sub.tx.send(ExitEvent {
                        subject: subject.clone(),
                        exit_code,
                    }) {
                        error!("failed to send exit code to subscriber {}: {:?}", i, e);
                        dead_subscribers.push(*i);
                    }
                }
            }
        }
        for id in dead_subscribers {
            let _ = self.unsubscribe(id);
        }
    }

    pub fn unsubscribe(&mut self, id: i64) -> Result<()> {
        let sub = self.subscribers.remove(&id);
        if let Some(s) = sub {
            self.topic_subs.get_mut(&s.topic).map(|v| {
                v.iter().position(|&x| x == id).map(|i| {
                    v.remove(i);
                })
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lazy_static::lazy_static;
    use tokio::sync::Mutex;

    use crate::{
        asynchronous::monitor::{
            monitor_notify_by_exec, monitor_notify_by_pid, monitor_subscribe, monitor_unsubscribe,
            MONITOR,
        },
        monitor::{Subject, Topic},
    };

    lazy_static! {
        // Use tokio::sync::Mutex for tests to avoid holding std::sync::MutexGuard across awaits
        static ref TEST_LOCK: Mutex<()> = Mutex::new(());
    }

    #[tokio::test]
    async fn test_monitor_table() {
        let _guard = TEST_LOCK.lock().await;
        // Clean up any leftovers from previous tests
        {
            let mut monitor = MONITOR.lock().unwrap();
            monitor.subscribers.clear();
            monitor.topic_subs.clear();
        }

        struct TestCase {
            name: &'static str,
            subscribe_to: Topic,
            notify_pid: Option<i32>,
            notify_exec: Option<(&'static str, &'static str)>,
            expected_pid: Option<i32>,
            expected_exec: Option<(&'static str, &'static str)>,
        }

        let cases = vec![
            TestCase {
                name: "pid_topic_receives_pid_event",
                subscribe_to: Topic::Pid,
                notify_pid: Some(101),
                notify_exec: None,
                expected_pid: Some(101),
                expected_exec: None,
            },
            TestCase {
                name: "pid_topic_ignores_exec_event",
                subscribe_to: Topic::Pid,
                notify_pid: None,
                notify_exec: Some(("c1", "e1")),
                expected_pid: None,
                expected_exec: None,
            },
            TestCase {
                name: "exec_topic_receives_exec_event",
                subscribe_to: Topic::Exec,
                notify_pid: None,
                notify_exec: Some(("c2", "e2")),
                expected_pid: None,
                expected_exec: Some(("c2", "e2")),
            },
            TestCase {
                name: "all_topic_receives_both",
                subscribe_to: Topic::All,
                notify_pid: Some(102),
                notify_exec: Some(("c3", "e3")),
                expected_pid: Some(102),
                expected_exec: Some(("c3", "e3")),
            },
        ];

        for tc in cases {
            let mut s = monitor_subscribe(tc.subscribe_to.clone()).await.unwrap();

            if let Some(pid) = tc.notify_pid {
                monitor_notify_by_pid(pid, 0).await.unwrap();
            }
            if let Some((cid, eid)) = tc.notify_exec {
                monitor_notify_by_exec(cid, eid, 0).await.unwrap();
            }

            if let Some(exp_pid) = tc.expected_pid {
                let event = tokio::time::timeout(Duration::from_millis(200), s.rx.recv())
                    .await
                    .unwrap_or_else(|_| panic!("{}: timed out waiting for pid", tc.name))
                    .expect("channel closed");
                match event.subject {
                    Subject::Pid(p) => assert_eq!(p, exp_pid, "{}", tc.name),
                    _ => panic!("{}: expected pid, got {:?}", tc.name, event.subject),
                }
            }

            if let Some((exp_cid, exp_eid)) = tc.expected_exec {
                let event = tokio::time::timeout(Duration::from_millis(200), s.rx.recv())
                    .await
                    .unwrap_or_else(|_| panic!("{}: timed out waiting for exec", tc.name))
                    .expect("channel closed");
                match event.subject {
                    Subject::Exec(c, e) => {
                        assert_eq!(c, exp_cid, "{}", tc.name);
                        assert_eq!(e, exp_eid, "{}", tc.name);
                    }
                    _ => panic!("{}: expected exec, got {:?}", tc.name, event.subject),
                }
            }

            // Ensure no extra messages
            let res = tokio::time::timeout(Duration::from_millis(50), s.rx.recv()).await;
            assert!(res.is_err(), "{}: received unexpected extra event", tc.name);

            monitor_unsubscribe(s.id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_monitor_backpressure() {
        let _guard = TEST_LOCK.lock().await;
        // Clean up
        {
            let mut monitor = MONITOR.lock().unwrap();
            monitor.subscribers.clear();
            monitor.topic_subs.clear();
        }

        let mut s = monitor_subscribe(Topic::Pid).await.unwrap();
        let sid = s.id;
        let count = 200;
        let base_pid = 10000;

        let receiver = tokio::spawn(async move {
            let mut received = 0;
            while received < count {
                if let Some(event) = s.rx.recv().await {
                    match event.subject {
                        Subject::Pid(pid) => {
                            assert_eq!(pid, base_pid + received);
                        }
                        _ => continue,
                    }
                    received += 1;
                    if received % 10 == 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                } else {
                    break;
                }
            }
            received
        });

        for i in 0..count {
            monitor_notify_by_pid(base_pid + i, 0).await.unwrap();
        }

        let received_count = tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .expect("Test timed out")
            .expect("Receiver task failed");

        assert_eq!(received_count, count);
        monitor_unsubscribe(sid).await.unwrap();
    }

    #[tokio::test]
    async fn test_monitor_reliability() {
        let _guard = TEST_LOCK.lock().await;
        // Clean up
        {
            let mut monitor = MONITOR.lock().unwrap();
            monitor.subscribers.clear();
            monitor.topic_subs.clear();
        }

        enum Action {
            Unsubscribe,
            Drop,
        }

        struct ReliabilityCase {
            name: &'static str,
            action: Action,
        }

        let cases = vec![
            ReliabilityCase {
                name: "explicit_unsubscribe",
                action: Action::Unsubscribe,
            },
            ReliabilityCase {
                name: "drop_handle",
                action: Action::Drop,
            },
        ];

        for tc in cases {
            let s_to_remove = monitor_subscribe(Topic::Pid).await.unwrap();
            let mut s_stay = monitor_subscribe(Topic::Pid).await.unwrap();
            let rid = s_to_remove.id;
            let test_pid = 20000;

            match tc.action {
                Action::Unsubscribe => {
                    monitor_unsubscribe(rid).await.unwrap();
                }
                Action::Drop => {
                    drop(s_to_remove);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }

            monitor_notify_by_pid(test_pid, 0).await.unwrap();

            // s_stay should receive the event
            let event = tokio::time::timeout(Duration::from_millis(500), s_stay.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("{}: stay subscription timed out", tc.name))
                .expect("channel closed");

            match event.subject {
                Subject::Pid(p) => assert_eq!(p, test_pid, "{}", tc.name),
                _ => panic!("{}: unexpected event", tc.name),
            }

            {
                let monitor = MONITOR.lock().unwrap();
                assert!(!monitor.subscribers.contains_key(&rid), "{}", tc.name);
            }
            monitor_unsubscribe(s_stay.id).await.unwrap();
        }
    }
}
