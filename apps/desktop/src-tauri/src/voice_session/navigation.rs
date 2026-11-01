use super::*;
use crate::voice::channel::types::ToolResult;
use crate::voice::tools::dispatcher::{PreparedNavigation, ToolOutcome};
use std::time::Duration;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationDisposition {
    Applied,
    Resume,
    Held,
    Unavailable,
    Superseded,
}

#[derive(Clone)]
pub(super) struct PendingNavigation {
    settled_applied: bool,
    call_id: String,
    tool: String,
    generation: u64,
    page: i64,
    plan: PreparedNavigation,
    deadline: tokio::time::Instant,
}

impl SessionRuntime {
    pub(super) fn navigation_deadline(&self) -> tokio::time::Instant {
        self.pending_navigation
            .as_ref()
            .map(|n| n.deadline)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600))
    }

    pub(super) async fn begin_navigation(
        &mut self,
        call_id: &str,
        tool: &str,
        args: &serde_json::Value,
        sink: &mut dyn SessionSink,
    ) -> Result<(), ChannelError> {
        if self
            .pending_navigation
            .as_ref()
            .is_some_and(|n| n.call_id == call_id)
        {
            return Ok(());
        }
        if self.dispatcher.pending_answer(call_id).is_some() {
            crate::voice::tools::answer_once(
                &mut self.dispatcher,
                self.channel.as_mut(),
                call_id,
                |_| unreachable!(),
            )
            .await?;
            return Ok(());
        }
        let prepared = if self.pending_navigation.is_some() {
            Err(ToolOutcome::Failed {
                code: "navigation_busy".into(),
                message: "A page change is awaiting the reader. Wait for its result.".into(),
            })
        } else if self.pump.response_generation() != self.generation.current() {
            Err(ToolOutcome::Failed {
                code: "stale_generation".into(),
                message: "This response was superseded. The reader did not move.".into(),
            })
        } else {
            self.dispatcher.prepare_navigation(
                tool,
                args,
                std::time::Instant::now(),
                self.playback.as_ref(),
                &mut self.excerpts,
            )
        };
        let plan = match prepared {
            Ok(plan) => plan,
            Err(outcome) => {
                self.finish_navigation_result(call_id, tool, outcome, false, sink)
                    .await?;
                return Ok(());
            }
        };
        let page = plan
            .page()
            .or_else(|| navigation_target(self.excerpts.snapshot(), plan.position()).0);
        let Some(page) = page.filter(|p| *p > 0) else {
            self.finish_navigation_result(
                call_id,
                tool,
                ToolOutcome::Failed {
                    code: "unresolved_page".into(),
                    message: "The target has no page the reader can display.".into(),
                },
                false,
                sink,
            )
            .await?;
            return Ok(());
        };
        self.return_navigation = None;
        let generation = self.generation.current();
        let position = plan.position();
        self.pending_navigation = Some(PendingNavigation {
            settled_applied: false,
            call_id: call_id.into(),
            tool: tool.into(),
            generation,
            page,
            plan,
            deadline: tokio::time::Instant::now() + Duration::from_secs(10),
        });
        sink.ui(SessionUiEvent::NavigationRequested {
            call_id: call_id.into(),
            generation,
            tool: tool.into(),
            page_no: page,
            pos_ms: position,
        });
        Ok(())
    }

    pub(super) async fn settle_navigation(
        &mut self,
        call_id: &str,
        generation: u64,
        page: i64,
        disposition: NavigationDisposition,
        sink: &mut dyn SessionSink,
    ) {
        let matches = |n: &PendingNavigation| {
            n.call_id == call_id && n.generation == generation && n.page == page
        };
        if matches!(disposition, NavigationDisposition::Resume) {
            let retained = self
                .return_navigation
                .as_ref()
                .filter(|n| matches(n))
                .cloned();
            let tool = retained
                .as_ref()
                .map(|n| n.tool.clone())
                .unwrap_or_else(|| "jump_to_page".into());
            let applied = if let Some(retained) = retained {
                matches!(
                    self.dispatcher.resume_navigation(
                        retained.plan,
                        self.playback.as_mut(),
                        &mut self.excerpts
                    ),
                    ToolOutcome::Ok(_)
                )
            } else {
                false
            };
            if applied {
                self.last_tool_outcome = None;
                self.dispatcher.set_shown_page(Some(page));
                if let Ok(pos) = self.playback.position_ms() {
                    self.last_chapter_idx = refresh_chapter_at(self.excerpts.snapshot(), pos);
                    let _ = self.journal.record_playback_pos(pos).await;
                }
            }
            sink.ui(SessionUiEvent::NavigationSettled {
                call_id: call_id.into(),
                tool,
                applied,
                held: false,
            });
            return;
        }
        if !self.pending_navigation.as_ref().is_some_and(matches) {
            return;
        }
        #[cfg(feature = "book-agent")]
        if self.egress_blocked().await {
            self.pending_navigation = None;
            return;
        }
        let pending = self
            .pending_navigation
            .take()
            .expect("matching pending navigation");
        let tool = pending.tool.clone();
        let stale = generation != self.generation.current();
        let held = !stale && matches!(disposition, NavigationDisposition::Held);
        let outcome = if !stale && matches!(disposition, NavigationDisposition::Applied) {
            self.dispatcher.set_shown_page(Some(page));
            let retained = PendingNavigation {
                settled_applied: true,
                ..pending.clone()
            };
            let result = self.dispatcher.commit_navigation(
                pending.plan,
                std::time::Instant::now(),
                self.playback.as_mut(),
                &mut self.excerpts,
            );
            if matches!(result, ToolOutcome::Ok(_)) {
                self.return_navigation = Some(retained);
            }
            result
        } else {
            let outcome = ToolOutcome::Failed {
                code: if stale { "navigation_superseded" } else if held { "navigation_held" } else { "navigation_not_applied" }.into(),
                message: format!("The reader did not display document page {page}. {} Do not claim the screen moved. For PDFs, page numbers refer to physical PDF pages, not printed book labels.", if held { "The user explicitly kept their page. Wait for them to return to the tutor." } else { "The move was interrupted or the page could not be displayed." }),
            };
            if held {
                self.return_navigation = Some(pending);
            }
            outcome
        };
        if let Err(error) = self
            .finish_navigation_result(call_id, &tool, outcome, held, sink)
            .await
        {
            self.recovering = self.on_send_failure(error, sink);
        }
    }

    async fn finish_navigation_result(
        &mut self,
        call_id: &str,
        tool: &str,
        outcome: ToolOutcome,
        held: bool,
        sink: &mut dyn SessionSink,
    ) -> Result<(), ChannelError> {
        let applied = matches!(outcome, ToolOutcome::Ok(_));
        self.last_tool_outcome = Some(RecentToolOutcome {
            tool: tool.into(),
            detail: if applied {
                "The reader confirmed the requested page".into()
            } else {
                "The requested page was not displayed".into()
            },
            succeeded: applied,
            at: tokio::time::Instant::now(),
        });
        let result = outcome.into_tool_result();
        let code = match &result {
            ToolResult::Err { code, .. } => Some(code.as_str()),
            _ => None,
        };
        let pos = applied.then(|| self.playback.position_ms().ok()).flatten();
        let _ = self
            .journal
            .record_tool_outcome(tool, call_id, applied, code, pos)
            .await;
        if let Some(pos) = pos {
            self.last_chapter_idx = refresh_chapter_at(self.excerpts.snapshot(), pos);
            let _ = self.journal.record_playback_pos(pos).await;
        }
        sink.ui(SessionUiEvent::NavigationSettled {
            call_id: call_id.into(),
            tool: tool.into(),
            applied,
            held,
        });
        let stale = matches!(&result, ToolResult::Err { code, .. } if code == "navigation_superseded" || code == "stale_generation");
        if !stale {
            self.activity("thinking", "after_tool", sink);
        }
        crate::voice::tools::answer_once(
            &mut self.dispatcher,
            self.channel.as_mut(),
            call_id,
            |_| result,
        )
        .await?;
        if !stale {
            self.tool_reply_watch = Some(ToolReplyWatch {
                deadline: tokio::time::Instant::now() + self.tool_deadline(),
                tool: tool.into(),
                armed_gen: self.generation.current(),
            });
        }
        Ok(())
    }

    pub(super) async fn cancel_stale_navigation(&mut self, sink: &mut dyn SessionSink) {
        if self
            .pending_navigation
            .as_ref()
            .is_some_and(|n| n.generation != self.generation.current())
        {
            self.expire_navigation(sink).await;
        }
        if self
            .return_navigation
            .as_ref()
            .is_some_and(|n| !n.settled_applied && n.generation != self.generation.current())
        {
            self.clear_return_navigation(sink);
        }
    }

    pub(super) fn clear_return_navigation(&mut self, sink: &mut dyn SessionSink) {
        if let Some(held) = self.return_navigation.take() {
            sink.ui(SessionUiEvent::NavigationSettled {
                call_id: held.call_id,
                tool: held.tool,
                applied: false,
                held: false,
            });
        }
    }

    pub(super) async fn expire_navigation(&mut self, sink: &mut dyn SessionSink) {
        if let Some(n) = &self.pending_navigation {
            let (id, generation, page) = (n.call_id.clone(), n.generation, n.page);
            self.settle_navigation(
                &id,
                generation,
                page,
                NavigationDisposition::Unavailable,
                sink,
            )
            .await;
        }
    }
}
