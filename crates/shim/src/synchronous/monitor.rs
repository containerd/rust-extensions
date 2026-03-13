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

use std::{
    collections::HashMap,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
    time::Duration,
};

use lazy_static::lazy_static;
use log::{error, warn};

use crate::{
    monitor::{ExitEvent, Subject, Topic},
    Result,
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

pub fn monitor_subscribe(topic: Topic) -> Result<Subscription> {
    let mut monitor = MONITOR.lock().unwrap();
    let s = monitor.subscribe(topic)?;
    Ok(s)
}

pub fn monitor_notify_by_pid(pid: i32, exit_code: i32) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    let subject = Subject::Pid(pid);
    monitor.notify_topic(&Topic::Pid, &subject, exit_code);
    monitor.notify_topic(&Topic::All, &subject, exit_code);
    Ok(())
}

pub fn monitor_notify_by_exec(id: &str, exec_id: &str, exit_code: i32) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    let subject = Subject::Exec(id.into(), exec_id.into());
    monitor.notify_topic(&Topic::Exec, &subject, exit_code);
    monitor.notify_topic(&Topic::All, &subject, exit_code);
    Ok(())
}

pub fn monitor_unsubscribe(id: i64) -> Result<()> {
    let mut monitor = MONITOR.lock().unwrap();
    monitor.unsubscribe(id)
}

pub struct Monitor {
    pub(crate) seq_id: i64,
    pub(crate) subscribers: HashMap<i64, Subscriber>,
    pub(crate) topic_subs: HashMap<Topic, Vec<i64>>,
}

pub(crate) struct Subscriber {
    pub(crate) topic: Topic,
    pub(crate) tx: Sender<ExitEvent>,
}

pub struct Subscription {
    pub id: i64,
    pub rx: Receiver<ExitEvent>,
}

impl Monitor {
    pub fn subscribe(&mut self, topic: Topic) -> Result<Subscription> {
        let (tx, rx) = channel::<ExitEvent>();
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
        if let Some(subs) = self.topic_subs.get(topic) {
            for i in subs {
                if let Some(sub) = self.subscribers.get(i) {
                    // channel::Sender::send is non-blocking when using unbounded channel.
                    // Sending while holding the lock prevents races with unsubscribe.
                    if let Err(e) = sub.tx.send(ExitEvent {
                        subject: subject.clone(),
                        exit_code,
                    }) {
                        warn!("failed to send exit event to subscriber {}: {}", i, e);
                    }
                }
            }
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

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut monitor = MONITOR.lock().unwrap();
        monitor.unsubscribe(self.id).unwrap_or_else(|e| {
            error!("failed to unsubscribe the subscription {}, {}", self.id, e);
        });
    }
}

pub fn wait_pid(pid: i32, s: Subscription) -> i32 {
    loop {
        match s.rx.recv_timeout(Duration::from_secs(1)) {
            Ok(ExitEvent {
                subject: Subject::Pid(epid),
                exit_code: code,
            }) => {
                if pid == epid {
                    return code;
                }
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return 128,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    lazy_static! {
        static ref SYNC_TEST_LOCK: Mutex<()> = Mutex::new(());
    }

    #[test]
    fn test_monitor_table_sync() {
        let _guard = SYNC_TEST_LOCK.lock().unwrap();
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
                notify_pid: Some(301),
                notify_exec: None,
                expected_pid: Some(301),
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
        ];

        for tc in cases {
            let s = monitor_subscribe(tc.subscribe_to.clone()).unwrap();

            if let Some(pid) = tc.notify_pid {
                monitor_notify_by_pid(pid, 0).unwrap();
            }
            if let Some((cid, eid)) = tc.notify_exec {
                monitor_notify_by_exec(cid, eid, 0).unwrap();
            }

            if let Some(exp_pid) = tc.expected_pid {
                let event =
                    s.rx.recv_timeout(Duration::from_millis(100))
                        .unwrap_or_else(|_| panic!("{}: timed out", tc.name));
                match event.subject {
                    Subject::Pid(p) => assert_eq!(p, exp_pid, "{}", tc.name),
                    _ => panic!("{}: unexpected subject", tc.name),
                }
            }

            if let Some((exp_cid, exp_eid)) = tc.expected_exec {
                let event =
                    s.rx.recv_timeout(Duration::from_millis(100))
                        .unwrap_or_else(|_| panic!("{}: timed out", tc.name));
                match event.subject {
                    Subject::Exec(c, e) => {
                        assert_eq!(c, exp_cid, "{}", tc.name);
                        assert_eq!(e, exp_eid, "{}", tc.name);
                    }
                    _ => panic!("{}: unexpected subject", tc.name),
                }
            }

            monitor_unsubscribe(s.id).unwrap();
        }
    }

    #[test]
    fn test_monitor_backpressure_sync() {
        let _guard = SYNC_TEST_LOCK.lock().unwrap();
        {
            let mut monitor = MONITOR.lock().unwrap();
            monitor.subscribers.clear();
            monitor.topic_subs.clear();
        }

        let s = monitor_subscribe(Topic::Pid).unwrap();
        let sid = s.id;
        let count = 200;
        let base_pid = 20000;

        let handle = thread::spawn(move || {
            let mut received = 0;
            while received < count {
                if let Ok(event) = s.rx.recv_timeout(Duration::from_secs(5)) {
                    match event.subject {
                        Subject::Pid(pid) => {
                            assert_eq!(pid, base_pid + received)
                        }
                        _ => continue,
                    }
                    received += 1;
                    if received % 10 == 0 {
                        thread::sleep(Duration::from_millis(1));
                    }
                } else {
                    break;
                }
            }
            received
        });

        for i in 0..count {
            monitor_notify_by_pid(base_pid + i, 0).unwrap();
        }

        let received_count = handle.join().expect("Receiver thread panicked");
        assert_eq!(received_count, count);

        monitor_unsubscribe(sid).unwrap();
    }

    #[test]
    fn test_monitor_reliability_sync() {
        let _guard = SYNC_TEST_LOCK.lock().unwrap();
        {
            let mut monitor = MONITOR.lock().unwrap();
            monitor.subscribers.clear();
            monitor.topic_subs.clear();
        }

        let s_to_drop = monitor_subscribe(Topic::Pid).unwrap();
        let s_stay = monitor_subscribe(Topic::Pid).unwrap();
        let rid = s_to_drop.id;
        let test_pid = 30000;

        drop(s_to_drop);
        thread::sleep(Duration::from_millis(50));

        monitor_notify_by_pid(test_pid, 0).unwrap();

        let event = s_stay.rx.recv_timeout(Duration::from_millis(200)).unwrap();
        match event.subject {
            Subject::Pid(p) => assert_eq!(p, test_pid),
            _ => panic!("unexpected event"),
        }

        let monitor = MONITOR.lock().unwrap();
        assert!(!monitor.subscribers.contains_key(&rid));
        drop(monitor);
        monitor_unsubscribe(s_stay.id).unwrap();
    }
}
