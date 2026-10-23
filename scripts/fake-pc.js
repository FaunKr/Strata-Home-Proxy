// The PC with Strata and the PC service for scripts/ui-check.sh, so the
// overview shows an awake PC with its Hardwarewerte and the Strata area its
// models and benchmarks, and the Version tab an update Strata summarizes. It also stands in for the code-bench runner, whose
// /health only needs to name opencode.
// POST /fake/summary?mode=hold|fail&upstream=<hash> moves upstream on, so the
// proxy asks for a new summary, and makes Strata hold or refuse the answer;
// a held request is refused once the mode changes. mode=answer without
// upstream goes back to the start.
// POST /fake/pc?state=awake|loading|hung|gaming|asleep sets what the PC service
// and Strata answer: loading runs Strata without answering yet, hung is past
// its load time (the PC service no longer counts it as running), asleep
// answers nothing.
// Sleep now is refused; a forced sleep puts the fake PC to sleep, and a forced
// Strata restart finds nothing to start.
// The answers start from the PC service's samples in the protocol crate, so a
// field added there reaches this check too; only what the check needs is set here.
// Usage: node scripts/fake-pc.js <port>
const fs = require('fs');
const http = require('http');
const path = require('path');

const sample = name =>
  JSON.parse(fs.readFileSync(path.join(__dirname, '../crates/pc-protocol/tests/samples', name), 'utf8'));

// No MAC: the check does not cover a reported one.
const status = { ...sample('status.json'), version: '0.1.0+ui-check', user_idle_secs: 0, mac: undefined };
const strataSample = sample('strata.json');

const at = Math.floor(Date.now() / 1000) - 3600;
const field = (name, value, source = 'model_card') => ({ name, value, source });
// Strata with the model card's sampling; one model has a temperature from the Chat settings.
const sampling = (temperature) => ({
  support: 'model_card',
  thinking: [temperature, field('top_p', 0.95), field('top_k', 20)],
  instruct: [field('temperature', 0.7), field('top_p', 0.8), field('top_k', 20), field('presence_penalty', 1.5)],
});
const model = (id, family, size, active, context, projection, temperature = field('temperature', 1)) => ({
  ...strataSample.models[0], id, family, size, disk_bytes: 60e9, active, speed_projection: projection, context,
  loop_recovery: 'recover', sampling: sampling(temperature), tunable: true, tune_unavailable: undefined,
});
const firstUpstream = 'fedcba9876543210fedcba9876543210fedcba98';
const strata = {
  ...strataSample,
  // The page's benchmark rows come from `benchmarks`.
  benchmark: undefined,
  installed: true,
  install: 'terminal',
  // The switch is done; the old Strata folder waits to be removed.
  old_strata_folder: true,
  terminal_served: true,
  version: '0.2.2 (build 0.1.44.1)',
  update_available: true,
  update: {
    upstream: firstUpstream,
    total: 3,
    remote: 'https://github.com/Niko1221/Strata.git',
    commits: [
      { hash: 'fedcba9876543210fedcba9876543210fedcba98', time: at, subject: 'Read long prompts in bigger chunks', body: 'Setup must run again.' },
      { hash: 'aaaabbbbccccddddeeeeffff0000111122223333', time: at - 86400, subject: 'Fix a crash when an answer hits max_tokens inside a tool call', body: '' },
      { hash: '0123456789abcdef0123456789abcdef01234567', time: at - 2 * 86400, subject: 'Docs: speed projection', body: '' },
    ],
    files: [
      { path: 'serve/server.py', added: 120, removed: 30 },
      { path: 'setup.py', added: 12, removed: 2 },
      { path: 'docs/speed.md', added: 40, removed: 0 },
    ],
  },
  models: [
    model('IQ3_XXS', 'qwen', 'IQ3_XXS', true, 65536, false),
    // The presets: the sample's active one, one that waits for Apply, none for a switched context.
    { ...model('coder-IQ1_M', 'coder', 'IQ1_M', false, 65536, true, field('temperature', 0.6, 'chat_settings')),
      presets: { latest: { ...strataSample.models[0].presets.latest, active: false, state: null, reasons: [] }, other_context: false } },
    { ...model('swift-IQ2_XS', 'swift', 'IQ2_XS', false, 32768, false), presets: { latest: null, other_context: true } },
  ],
  families: [
    { id: 'qwen', title: 'Qwen3.8-Flash-Next', speed_projection: true, pictures: true },
    { id: 'coder', title: 'Coder', speed_projection: true, pictures: true },
    { id: 'swift', title: 'Swift', speed_projection: false, pictures: true },
    { id: 'gyro', title: 'Qwen3.8-Flash-Next (Gyro)', speed_projection: false, pictures: true },
  ],
  catalog: [
    { id: 'IQ3_S', family: 'qwen', size: 'IQ3_S', download_gb: 83.6, ram_gb: 62, memory_with_vram: false, fits: true, partly_downloaded: false },
    { id: 'gyro-Gyro-S', family: 'gyro', size: 'Gyro-S', download_gb: 58.5, ram_gb: 28, memory_with_vram: true, fits: true, partly_downloaded: false },
    { id: 'gyro-Gyro-M', family: 'gyro', size: 'Gyro-M', download_gb: 92, ram_gb: 35, memory_with_vram: true, fits: false, partly_downloaded: false },
  ],
  recommended: 'IQ3_S',
  contexts: [32768, 65536, 131072],
  default_context: 65536,
  ram_gb: 64,
  disk_free_gb: 412,
  data_dir: 'D:\\Strata-data',
  benchmarks: [
    { model: 'IQ3_XXS', result: '38.5 Token/s', at, decode_tps: 38.5, prompt_tps: 612, context: 65536, speed_projection: false, version: '0.2.2 (build 0.1.44.1)', tool: 'strata_bench' },
    { model: 'coder-IQ1_M', result: '52.3 Token/s', at, decode_tps: 52.3, prompt_tps: 845.5, context: 65536, speed_projection: true, version: '477ac13' },
    { model: 'swift-IQ2_XS', result: '', at, context: 32768, speed_projection: false, version: '477ac13', error: 'BENCH.bat ended with code 1' },
  ],
  job: null,
};
const idleMetrics = 'vllm:num_requests_running{model_name="Qwen3-30B-A3B"} 0\nvllm:num_requests_waiting{model_name="Qwen3-30B-A3B"} 0\n';
let pcState = 'awake';
let summaryMode = 'answer';
// A run over all models: the fake accepts an Activate job and finishes it, so
// the card's progress can reach the first task of the first model; the runner
// reports that run with its first task running.
let strataJob = null;
let runnerCurrent = null;
const runFor = (model, state) => ({
  id: 1, model, state, error: null,
  started: Math.floor(Date.now() / 1000), ended: null,
  tasks: [{ name: 'roman', title: 'Roman numerals', state: state === 'running' ? 'running' : 'done',
    total_secs: 0, steps: 0, input_tokens: 0, output_tokens: 0, output_tps: null,
    passed: 0, total: 8, timed_out: false, files: {}, test_output: '', error: null }],
});
let held = [];
const summary = '- **Run setup again** after this update.\n- Long prompts are read faster.\n- A crash when an answer hits max_tokens in a tool call is fixed.';

http.createServer((req, res) => {
  const json = body => {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(body));
  };
  const refuse = body => {
    res.statusCode = 409;
    json(body);
  };
  if (pcState === 'asleep' && !req.url.startsWith('/fake/')) {
    res.statusCode = 503;
    res.end();
  }
  else if (req.url.startsWith('/status')) {
    // Only a tune is reported as running, so its card shows.
    const tuning = strataJob?.kind === 'tune' && strataJob.state === 'running';
    json({ ...status, gaming_mode: pcState === 'gaming', strata_running: pcState === 'awake' || pcState === 'loading',
      ...(tuning ? { strata_job: true, strata_job_kind: 'tune' } : {}) });
  }
  else if (req.url.startsWith('/strata/key')) {
    json(sample('strata-key.json'));
  }
  else if (req.url.startsWith('/health') && pcState !== 'awake') {
    res.statusCode = 503;
    res.end();
  }
  else if (req.method === 'POST' && req.url === '/sleep') {
    refuse({ ...sample('sleep-refused.json'), reason: 'Someone used the PC 2 min ago' });
  }
  else if (req.method === 'POST' && req.url === '/sleep/force') {
    json({ ...sample('sleep-force.json'), cancelled_job: undefined, strata_busy: false, gaming_mode: pcState === 'gaming' });
    pcState = 'asleep';
  }
  else if (req.method === 'POST' && req.url === '/strata/kill') {
    if (pcState === 'gaming') refuse(sample('strata-kill-refused.json'));
    else json({ ...sample('strata-kill.json'), cancelled_job: undefined, started: false });
  }
  else if (req.method === 'POST' && req.url.startsWith('/fake/pc')) {
    pcState = new URL(req.url, 'http://fake').searchParams.get('state');
    res.end();
  }
  else if (req.url.startsWith('/health')) json({ status: 'ok', loaded: true, service: 'strata', model: 'Qwen3-30B-A3B', max_context: 32768, opencode: '1.19.3' });
  else if (req.url === '/strata') json(strata);
  else if (req.method === 'POST' && req.url === '/strata/jobs') {
    let raw = '';
    req.on('data', (c) => { raw += c; });
    req.on('end', () => {
      const body = raw ? JSON.parse(raw) : {};
      strataJob = { id: 1, kind: 'activate', model: body.model ?? null, state: 'running',
        lines: [], error: null, started: Math.floor(Date.now() / 1000), ended: null };
      // A run over all models activates each model; end the job shortly after,
      // so the proxy moves on to that model's run.
      const job = strataJob;
      setTimeout(() => {
        if (strataJob === job) {
          strataJob = { ...job, state: 'done', ended: Math.floor(Date.now() / 1000) };
        }
      }, 1500);
      res.statusCode = 202;
      json(job);
    });
  }
  // A tune 12 minutes in with nothing measured yet, as a quick tune's status
  // file can stay to its end, for 8 s.
  else if (req.method === 'POST' && req.url === '/fake/tune-start') {
    const job = { id: 1, kind: 'tune', model: 'IQ3_XXS', state: 'running', step: 'Measuring your current settings',
      lines: ['[0m] Measuring your current settings'], error: null,
      started: Math.floor(Date.now() / 1000) - 12 * 60, ended: null };
    strataJob = job;
    setTimeout(() => {
      if (strataJob === job) strataJob = { ...job, state: 'done', ended: Math.floor(Date.now() / 1000) };
    }, 8000);
    res.end();
  }
  // A running tune with its step log and its measurements so far, for 8 s.
  else if (req.method === 'POST' && req.url === '/fake/tune') {
    const job = { id: 1, kind: 'tune', model: 'IQ3_XXS', state: 'running', percent: 8, eta_secs: 6569,
      step: 'Trying: Run the control thread on the last CPU core (round 1 of 3)',
      lines: ['[0m] Measuring your current settings', '[5m] Trying: Keep more of the conversation in graphics memory (round 1 of 3)',
        '[10m] Trying: Run the control thread on the last CPU core (round 1 of 3)'],
      speeds: [{ label: 'base', tok_s: 171.4 }, { label: 'Keep more of the conversation in graphics memory', tok_s: 172.2 }],
      error: null, started: Math.floor(Date.now() / 1000), ended: null };
    strataJob = job;
    setTimeout(() => {
      if (strataJob === job) strataJob = { ...job, state: 'done', ended: Math.floor(Date.now() / 1000) };
    }, 8000);
    res.end();
  }
  else if (req.url === '/strata/job') json(strataJob);
  else if (req.url.startsWith('/metrics')) res.end(idleMetrics);
  // The code-bench runner for a run over all models: a run with its first task
  // running, so the card's progress reaches task 1.
  else if (req.method === 'POST' && req.url === '/runs') {
    let raw = '';
    req.on('data', (c) => { raw += c; });
    req.on('end', () => {
      const body = raw ? JSON.parse(raw) : {};
      runnerCurrent = runFor(body.model || 'IQ3_XXS', 'running');
      res.statusCode = 202;
      json(runnerCurrent);
    });
  }
  else if (req.url === '/runs/current') {
    if (!runnerCurrent) {
      res.statusCode = 404;
      res.end();
    } else json(runnerCurrent);
  }
  else if (req.method === 'POST' && req.url === '/runs/current/cancel') {
    if (!runnerCurrent) {
      res.statusCode = 409;
      res.end();
    } else {
      runnerCurrent = { ...runnerCurrent, state: 'cancelled' };
      json(runnerCurrent);
    }
  }
  else if (req.url.startsWith('/v1/chat/completions')) {
    req.resume();
    req.on('end', () => {
      if (summaryMode === 'hold') held.push(res);
      else if (summaryMode === 'fail') res.writeHead(500).end();
      else json({ model: 'Qwen3-30B-A3B', choices: [{ message: { content: summary } }] });
    });
  }
  else if (req.method === 'POST' && req.url.startsWith('/fake/summary')) {
    const q = new URL(req.url, 'http://fake').searchParams;
    summaryMode = q.get('mode');
    strata.update.upstream = q.get('upstream') ?? firstUpstream;
    for (const r of held) r.writeHead(500).end();
    held = [];
    res.end();
  }
  else {
    res.statusCode = 404;
    res.end();
  }
}).listen(Number(process.argv[2]), '127.0.0.1');
