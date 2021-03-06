// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        error::ModelError,
        events::{dispatcher::EventDispatcher, event::SyncMode, stream::EventStream},
        hooks::{Event as ComponentEvent, EventType, Hook, HooksRegistration},
        moniker::AbsoluteMoniker,
    },
    async_trait::async_trait,
    cm_rust::CapabilityName,
    fuchsia_trace as trace,
    futures::lock::Mutex,
    std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Weak},
    },
};

pub struct RoutedEvent {
    pub source_name: CapabilityName,
    pub scope_monikers: HashSet<AbsoluteMoniker>,
}

/// Subscribes to events from multiple tasks and sends events to all of them.
pub struct EventRegistry {
    dispatcher_map: Arc<Mutex<HashMap<CapabilityName, Vec<Weak<EventDispatcher>>>>>,
}

impl EventRegistry {
    pub fn new() -> Self {
        Self { dispatcher_map: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![
            // This hook must be registered with all events.
            // However, a task will only receive events to which it subscribed.
            HooksRegistration::new(
                "EventRegistry",
                EventType::values(),
                Arc::downgrade(self) as Weak<dyn Hook>,
            ),
        ]
    }

    /// Subscribes to events of a provided set of EventTypes.
    pub async fn subscribe(&self, sync_mode: &SyncMode, events: Vec<RoutedEvent>) -> EventStream {
        // TODO(fxb/48510): get rid of this channel and use FIDL directly.
        let mut event_stream = EventStream::new();

        let mut dispatcher_map = self.dispatcher_map.lock().await;
        for event in events {
            let dispatchers = dispatcher_map.entry(event.source_name).or_insert(vec![]);
            let dispatcher =
                event_stream.create_dispatcher(sync_mode.clone(), event.scope_monikers);
            dispatchers.push(dispatcher);
        }

        event_stream
    }

    /// Sends the event to all dispatchers and waits to be unblocked by all
    async fn dispatch(&self, event: &ComponentEvent) -> Result<(), ModelError> {
        // Copy the senders so we don't hold onto the sender map lock
        // If we didn't do this, it is possible to deadlock while holding onto this lock.
        // For example,
        // Task A : call dispatch(event1) -> lock on sender map -> send -> wait for responders
        // Task B : call dispatch(event2) -> lock on sender map
        // If task B was required to respond to event1, then this is a deadlock.
        // Neither task can make progress.
        let dispatchers = {
            let mut dispatcher_map = self.dispatcher_map.lock().await;
            if let Some(dispatchers) = dispatcher_map.get_mut(&event.payload.type_().into()) {
                let mut strong_dispatchers = vec![];
                dispatchers.retain(|dispatcher| {
                    if let Some(dispatcher) = dispatcher.upgrade() {
                        strong_dispatchers.push(dispatcher);
                        true
                    } else {
                        false
                    }
                });
                strong_dispatchers
            } else {
                // There were no senders for this event. Do nothing.
                return Ok(());
            }
        };

        let mut responder_channels = vec![];
        for dispatcher in dispatchers {
            let result = dispatcher.dispatch(event.clone()).await;
            match result {
                Ok(Some(responder_channel)) => {
                    // A future can be canceled if the EventStream was dropped after
                    // a send. We don't crash the system when this happens. It is
                    // perfectly valid for a EventStream to be dropped. That simply
                    // means that the EventStream is no longer interested in future
                    // events. So we force each future to return a success. This
                    // ensures that all the futures can be driven to completion.
                    let responder_channel = async move {
                        trace::duration!("component_manager", "events:wait_for_resume");
                        let _ = responder_channel.await;
                        trace::flow_end!("component_manager", "event", event.id);
                    };
                    responder_channels.push(responder_channel);
                }
                // There's nothing to do if event is outside the scope of the given
                // `EventDispatcher`.
                Ok(None) => (),
                Err(_) => {
                    // A send can fail if the EventStream was dropped. We don't
                    // crash the system when this happens. It is perfectly
                    // valid for a EventStream to be dropped. That simply means
                    // that the EventStream is no longer interested in future
                    // events.
                }
            }
        }

        // Wait until all tasks have used the responder to unblock.
        {
            trace::duration!("component_manager", "events:wait_for_all_resume");
            futures::future::join_all(responder_channels).await;
        }

        Ok(())
    }

    #[cfg(test)]
    async fn dispatchers_per_event_type(&self, event_type: EventType) -> usize {
        let dispatcher_map = self.dispatcher_map.lock().await;
        dispatcher_map
            .get(&event_type.into())
            .map(|dispatchers| dispatchers.len())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Hook for EventRegistry {
    async fn on(self: Arc<Self>, event: &ComponentEvent) -> Result<(), ModelError> {
        self.dispatch(event).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::hooks::{Event as ComponentEvent, EventPayload},
        maplit::hashset,
    };

    async fn dispatch_fake_event(registry: &EventRegistry) -> Result<(), ModelError> {
        let root_component_url = "test:///root".to_string();
        let event = ComponentEvent::new(
            AbsoluteMoniker::root(),
            EventPayload::Discovered { component_url: root_component_url },
        );
        registry.dispatch(&event).await
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn drop_dispatcher_when_event_stream_dropped() {
        let event_registry = EventRegistry::new();

        assert_eq!(0, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        let mut event_stream_a = event_registry
            .subscribe(
                &SyncMode::Async,
                vec![RoutedEvent {
                    source_name: EventType::Discovered.into(),
                    scope_monikers: hashset!(AbsoluteMoniker::root()),
                }],
            )
            .await;

        assert_eq!(1, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        let mut event_stream_b = event_registry
            .subscribe(
                &SyncMode::Async,
                vec![RoutedEvent {
                    source_name: EventType::Discovered.into(),
                    scope_monikers: hashset!(AbsoluteMoniker::root()),
                }],
            )
            .await;

        assert_eq!(2, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        dispatch_fake_event(&event_registry).await.unwrap();

        // Verify that both EventStreams receive the event.
        assert!(event_stream_a.next().await.is_some());
        assert!(event_stream_b.next().await.is_some());
        assert_eq!(2, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        drop(event_stream_a);

        // EventRegistry won't drop EventDispatchers until an event is dispatched.
        assert_eq!(2, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        dispatch_fake_event(&event_registry).await.unwrap();

        assert!(event_stream_b.next().await.is_some());
        assert_eq!(1, event_registry.dispatchers_per_event_type(EventType::Discovered).await);

        drop(event_stream_b);

        dispatch_fake_event(&event_registry).await.unwrap();
        assert_eq!(0, event_registry.dispatchers_per_event_type(EventType::Discovered).await);
    }
}
