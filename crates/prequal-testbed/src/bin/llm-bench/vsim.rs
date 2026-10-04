//! Virtual-time simulation (`--virtual`): the HTTP benchmark's engine core, workload and routing library
//! (`prequal_llm::Scheduler`, one per router) driven by an event queue on a simulated clock, with no processes or
//! sockets. Routers scrape every engine every 50 ms of simulated time, like `scrape_forever`. A load ladder that
//! takes minutes in real time runs in seconds, deterministically per seed.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use prequal_llm::{
    PrefillSignal, Prompt, Scheduler, Ticket, policy,
    queue_order::{HistoryKeys, OutputHistory},
};

use crate::{
    diagnose::Diagnosis,
    engine::{EngineCore, Hidden, Job},
    load::{Completed, Outcome},
    order::{Orderer, QueueOrder},
    preset::{EngineSpec, Slowdown},
    workload::{self, Source, Stage},
};

const SCRAPE_US: u64 = 50_000;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Arrive { user: Option<usize> },
    StepEnd(usize),
    Timeout(usize),
    Scrape,
}

struct Req {
    arrived: u64,
    stage: usize,
    user: Option<usize>,
    ticket: Option<Ticket>,
    router: usize,
    keys: HistoryKeys,
    prompt_tokens: u64,
    output: u64,
    cached: u64,
    first: Option<u64>,
    outcome: Option<Outcome>,
}

pub struct Setup<'a> {
    pub specs: &'a [EngineSpec],
    pub policy: &'a str,
    pub routers: usize,
    pub stages: &'a [Stage],
    pub closed_users: Option<usize>,
    pub timeout: Duration,
    pub seed: u64,
    pub slowdown: Option<Slowdown>,
    pub signal: PrefillSignal,
    /// Tickets end at the first token, as when the gateway's ext_proc stream is closed there.
    pub end_at_first_token: bool,
    /// `--diagnose`: each popularity tier's exclusive end group.
    pub diagnose: Option<Vec<usize>>,
    pub hidden: Hidden,
    pub queue_order: Option<QueueOrder>,
}

struct Sim {
    now: Arc<AtomicU64>,
    events: BinaryHeap<Reverse<(u64, u64, Event)>>,
    seq: u64,
    /// Queued events other than scrapes; scraping stops once none are left.
    pending: usize,
    engines: Vec<EngineCore<usize>>,
    stepping: Vec<bool>,
    addrs: Vec<SocketAddr>,
    routers: Vec<Scheduler>,
    /// One per router when `--queue-order` is set; empty for FCFS.
    orderers: Vec<Orderer>,
    reqs: Vec<Req>,
    cancelled: HashSet<usize>,
    source: Source,
    bounds: Vec<u64>,
    timeout_us: u64,
    slowdown: Option<Slowdown>,
    end_at_first_token: bool,
    diagnosis: Option<Diagnosis>,
    hidden: Hidden,
}

/// Runs `setup` to completion; returns each stage's outcomes (all times in simulated µs) and per-engine
/// `(prompt tokens, cached tokens)` totals.
pub fn run(setup: &Setup, source: Source) -> (Vec<Vec<Outcome>>, Vec<(u64, u64)>) {
    let now = Arc::new(AtomicU64::new(0));
    let base = Instant::now();
    let clock_now = Arc::clone(&now);
    let clock: prequal_llm::Clock = Arc::new(move || base + Duration::from_micros(clock_now.load(Ordering::Relaxed)));
    let addrs: Vec<SocketAddr> =
        (0..setup.specs.len()).map(|i| SocketAddr::from(([10, 0, 0, i as u8 + 1], 8000))).collect();
    let diagnosis = setup.diagnose.clone().map(|tier_ends| Diagnosis::new(addrs.clone(), tier_ends));
    let routers: Vec<Scheduler> = (0..setup.routers.max(1))
        .map(|r| {
            let policy = policy::by_name(setup.policy).unwrap_or_else(|| panic!("unknown policy {}", setup.policy));
            let policy = match &diagnosis {
                Some(d) => d.wrap(policy),
                None => policy,
            };
            let scheduler = Scheduler::new(policy)
                .with_clock(Arc::clone(&clock))
                .with_seed(setup.seed ^ r as u64)
                .with_prefill_signal(setup.signal);
            scheduler.sync(addrs.iter().copied());
            scheduler
        })
        .collect();
    let bounds = setup.stages.iter().scan(0, |end, s| {
        *end += s.duration.as_micros() as u64;
        Some(*end)
    });
    let mut sim = Sim {
        now,
        events: BinaryHeap::new(),
        seq: 0,
        pending: 0,
        engines: setup.specs.iter().map(|&spec| EngineCore::new(EngineSpec { time_scale: 1.0, ..spec })).collect(),
        stepping: vec![false; setup.specs.len()],
        addrs,
        orderers: setup
            .queue_order
            .iter()
            .flat_map(|&order| (0..routers.len()).map(move |r| Orderer::new(order, setup.seed ^ r as u64)))
            .collect(),
        routers,
        reqs: Vec::new(),
        cancelled: HashSet::new(),
        source,
        bounds: bounds.collect(),
        timeout_us: setup.timeout.as_micros() as u64,
        slowdown: setup.slowdown,
        end_at_first_token: setup.end_at_first_token,
        diagnosis,
        hidden: setup.hidden,
    };
    sim.schedule_arrivals(setup);
    sim.push(0, Event::Scrape);
    while let Some(Reverse((at, _, event))) = sim.events.pop() {
        sim.now.store(at, Ordering::Relaxed);
        sim.pending -= usize::from(event != Event::Scrape);
        match event {
            Event::Arrive { user } => sim.arrive(user),
            Event::StepEnd(engine) => sim.step_end(engine),
            Event::Timeout(id) => sim.expire(id),
            Event::Scrape => sim.scrape(),
        }
    }
    sim.diagnosis.iter().for_each(Diagnosis::print);
    println!("preemptions: {}", sim.engines.iter().map(|e| e.preemptions).sum::<u64>());
    let mut stages = vec![Vec::new(); setup.stages.len()];
    for req in sim.reqs {
        stages[req.stage].push(req.outcome.unwrap_or(Err(())));
    }
    (stages, sim.engines.iter().map(|e| (e.prefix_queries, e.prefix_hits)).collect())
}

impl Sim {
    fn now(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }

    fn push(&mut self, at: u64, event: Event) {
        self.seq += 1;
        self.pending += usize::from(event != Event::Scrape);
        self.events.push(Reverse((at, self.seq, event)));
    }

    fn stage_at(&self, at: u64) -> Option<usize> {
        self.bounds.iter().position(|&end| at < end)
    }

    fn schedule_arrivals(&mut self, setup: &Setup) {
        if let Some(users) = setup.closed_users {
            (0..users).for_each(|u| self.push(0, Event::Arrive { user: Some(u) }));
            return;
        }
        for at in self.source.arrivals_us(setup.stages, setup.seed) {
            self.push(at, Event::Arrive { user: None });
        }
    }

    fn arrive(&mut self, user: Option<usize>) {
        let now = self.now();
        let Some(stage) = self.stage_at(now) else { return };
        let request = self.source.next();
        let text = match &request.prompt {
            workload::Prompt::Text(text) => text.as_str(),
            workload::Prompt::Tokens(_) => "",
        };
        let id = self.reqs.len();
        let body = format!(r#"{{"max_tokens":{},"prompt":"{text}"}}"#, request.max_tokens);
        let router = id % self.routers.len();
        let routed = Prompt::from_body(body.as_bytes());
        let mut ticket =
            self.routers[router].route(&routed, request.max_tokens, |_| true).expect("replicas are synced");
        // vLLM sends response headers before it even queues the request.
        ticket.response_started();
        let engine = self.addrs.iter().position(|a| *a == ticket.addr()).expect("ticket names a replica");
        if let Some(d) = &mut self.diagnosis {
            d.observe(request.group, text.as_bytes(), engine, &self.engines);
        }
        let (hashes, tokens) = match request.prompt {
            workload::Prompt::Text(_) => self.engines[engine].tokenize(text.as_bytes()),
            workload::Prompt::Tokens(n) => (Vec::new(), n),
        };
        let keys = OutputHistory::keys(&routed);
        let prompt = tokens.max(1);
        let priority = self.orderers.get_mut(router).map_or(0, |o| o.priority(now, &routed, request.max_tokens));
        let job = Job::new(hashes, prompt, request.max_tokens, priority, id);
        self.reqs.push(Req {
            arrived: now,
            stage,
            user,
            ticket: Some(ticket),
            router,
            keys,
            prompt_tokens: prompt,
            output: request.max_tokens,
            cached: 0,
            first: None,
            outcome: None,
        });
        self.engines[engine].submit(job);
        self.push(now + self.timeout_us, Event::Timeout(id));
        if !self.stepping[engine] {
            self.start_step(engine);
        }
    }

    fn start_step(&mut self, engine: usize) {
        let reqs = &mut self.reqs;
        let step = self.engines[engine].start_step(|&id, cached| reqs[id].cached = cached);
        self.stepping[engine] = step.is_some();
        if let Some(us) = step {
            let us = us * Slowdown::at(self.slowdown, self.now());
            let at = self.now() + (us as u64).max(1);
            self.push(at, Event::StepEnd(engine));
        }
    }

    fn step_end(&mut self, engine: usize) {
        let now = self.now();
        let (reqs, cancelled, end_at_first) = (&mut self.reqs, &self.cancelled, self.end_at_first_token);
        let done = self.engines[engine].finish_step(
            |&id, first| {
                let req = &mut reqs[id];
                if first && let Some(ticket) = req.ticket.as_mut() {
                    ticket.first_token();
                    req.first = Some(now);
                    if end_at_first {
                        req.ticket = None;
                    }
                }
            },
            |id| cancelled.contains(id),
        );
        for job in done {
            if !self.cancelled.contains(job.tag()) {
                self.finish(*job.tag(), true);
            }
        }
        self.start_step(engine);
    }

    fn expire(&mut self, id: usize) {
        if self.reqs[id].outcome.is_none() {
            self.cancelled.insert(id);
            self.finish(id, false);
        }
    }

    /// Records the request's outcome, releases its routing ticket, and lets a closed-loop user send again.
    fn finish(&mut self, id: usize, ok: bool) {
        let now = self.now();
        let req = &mut self.reqs[id];
        let user = req.user;
        req.ticket = None;
        if ok && let Some(orderer) = self.orderers.get_mut(req.router) {
            orderer.observe(&req.keys, req.output);
        }
        req.outcome = Some(match (ok, req.first) {
            (true, Some(first)) => Ok(Completed {
                ttft_us: first - req.arrived,
                e2e_us: now - req.arrived,
                tokens: req.output,
                prefix: Some((req.prompt_tokens, req.cached)),
            }),
            _ => Err(()),
        });
        if let Some(user) = user {
            self.push(now, Event::Arrive { user: Some(user) });
        }
    }

    fn scrape(&mut self) {
        for (engine, addr) in self.engines.iter().zip(&self.addrs) {
            let stats = engine.stats(self.hidden);
            self.routers.iter().for_each(|r| r.observe(*addr, stats));
        }
        if self.pending > 0 {
            let at = self.now() + SCRAPE_US;
            self.push(at, Event::Scrape);
        }
    }
}
