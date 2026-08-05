use std::collections::VecDeque;

/// Presentation identity is deliberately separate from execution state.  A
/// bounded record lets live, archive, and static projections refer to the
/// same semantic block without retaining a second copy of its body.
pub(crate) type PresentationId = u64;

/// 64 live blocks + bounded tool/reasoning/answer archives, with headroom for
/// a task boundary; still small enough to keep snapshots cheap.
pub(crate) const MAX_PRESENTATION_RECORDS: usize = 192;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PresentationMetrics {
    pub(crate) step: usize,
    pub(crate) elapsed_s: u64,
    pub(crate) tokens: usize,
    pub(crate) chars: usize,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum PresentationChannel {
    Answer,
    Reasoning,
    Tool,
}

impl PresentationChannel {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Reasoning => "reasoning",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresentationStatus {
    Live,
    Committed,
    Partial,
    Archived,
}

impl PresentationStatus {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Committed => "committed",
            Self::Partial => "partial",
            Self::Archived => "archived",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PresentationRecord {
    pub(crate) id: PresentationId,
    pub(crate) channel: PresentationChannel,
    pub(crate) status: PresentationStatus,
    pub(crate) step: usize,
    pub(crate) elapsed_s: u64,
    pub(crate) tokens: usize,
    pub(crate) chars: usize,
}

#[derive(Debug)]
pub(crate) struct PresentationLedger {
    next_id: PresentationId,
    records: VecDeque<PresentationRecord>,
}

impl Default for PresentationLedger {
    fn default() -> Self {
        Self {
            next_id: 0,
            records: VecDeque::with_capacity(MAX_PRESENTATION_RECORDS),
        }
    }
}

impl PresentationLedger {
    pub(crate) fn allocate(
        &mut self,
        channel: PresentationChannel,
        metrics: PresentationMetrics,
    ) -> PresentationId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.push(PresentationRecord {
            id,
            channel,
            status: PresentationStatus::Live,
            step: metrics.step,
            elapsed_s: metrics.elapsed_s,
            tokens: metrics.tokens,
            chars: metrics.chars,
        });
        id
    }

    pub(crate) fn contains(&self, channel: PresentationChannel, id: PresentationId) -> bool {
        self.records
            .iter()
            .any(|record| record.channel == channel && record.id == id)
    }

    pub(crate) fn status(
        &self,
        channel: PresentationChannel,
        id: PresentationId,
    ) -> Option<PresentationStatus> {
        self.records
            .iter()
            .find(|record| record.channel == channel && record.id == id)
            .map(|record| record.status)
    }

    pub(crate) fn metrics(
        &self,
        channel: PresentationChannel,
        id: PresentationId,
    ) -> Option<PresentationMetrics> {
        self.records
            .iter()
            .find(|record| record.channel == channel && record.id == id)
            .map(|record| PresentationMetrics {
                step: record.step,
                elapsed_s: record.elapsed_s,
                tokens: record.tokens,
                chars: record.chars,
            })
    }

    pub(crate) fn touch(
        &mut self,
        channel: PresentationChannel,
        id: PresentationId,
        metrics: PresentationMetrics,
    ) {
        if let Some(record) = self.find_mut(channel, id) {
            record.step = metrics.step;
            record.elapsed_s = metrics.elapsed_s;
            record.tokens = metrics.tokens;
            record.chars = metrics.chars;
        }
    }

    pub(crate) fn settle(
        &mut self,
        channel: PresentationChannel,
        id: PresentationId,
        status: PresentationStatus,
        metrics: PresentationMetrics,
    ) {
        if let Some(record) = self.find_mut(channel, id) {
            record.status = status;
            record.step = metrics.step;
            record.elapsed_s = metrics.elapsed_s;
            record.tokens = metrics.tokens;
            record.chars = metrics.chars;
        }
    }

    pub(crate) fn archive(&mut self, channel: PresentationChannel, id: PresentationId) {
        if let Some(record) = self.find_mut(channel, id) {
            record.status = PresentationStatus::Archived;
        }
    }

    pub(crate) fn discard(&mut self, channel: PresentationChannel, id: PresentationId) {
        if let Some(index) = self
            .records
            .iter()
            .position(|record| record.channel == channel && record.id == id)
        {
            self.records.remove(index);
        }
    }

    pub(crate) fn records(&self) -> &VecDeque<PresentationRecord> {
        &self.records
    }

    fn find_mut(
        &mut self,
        channel: PresentationChannel,
        id: PresentationId,
    ) -> Option<&mut PresentationRecord> {
        self.records
            .iter_mut()
            .find(|record| record.channel == channel && record.id == id)
    }

    fn push(&mut self, record: PresentationRecord) {
        if self.records.len() == MAX_PRESENTATION_RECORDS {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }
}
