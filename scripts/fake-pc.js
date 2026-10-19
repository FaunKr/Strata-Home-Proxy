// The PC with Strata and the PC service for scripts/ui-check.sh, so the
// overview shows an awake PC with its Hardwarewerte and the Strata area its
// models and benchmarks, and the Version tab an update Strata summarizes. It also stands in for the code-bench runner, whose
// /health only needs to name opencode.
// POST /fake/summary?mode=hold|fail&upstream=<hash> moves upstream on, so the
// proxy asks for a new summary, and makes Strata hold or refuse the answer;
// a held request is refused once the mode changes. mode=answer without
// upstream goes back to the start.
// Usage: node scripts/fake-pc.js <port>
const http = require('http');

const status = {
  gaming_mode: false,
  strata_running: true,
  strata_job: false,
  version: '0.1.0+ui-check',
  hardware: {
    gpu: { load_pct: 37.0, temp_c: 61.5, vram_used_bytes: 12670000000, vram_total_bytes: 17163091968, fan_rpm: 0, power_pct: 41.0 },
    cpu: { load_pct: 12.0, ram_used_bytes: 22980000000, ram_total_bytes: 34359738368 },
  },
};

const at = Math.floor(Date.now() / 1000) - 3600;
const field = (name, value, source = 'model_card') => ({ name, value, source });
// Strata with the model card's sampling; one model has a temperature from the Chat settings.
const sampling = (temperature) => ({
  support: 'model_card',
  thinking: [temperature, field('top_p', 0.95), field('top_k', 20)],
  instruct: [field('temperature', 0.7), field('top_p', 0.8), field('top_k', 20), field('presence_penalty', 1.5)],
});
const model = (id, family, size, active, context, projection, temperature = field('temperature', 1)) => ({
  id, family, size, disk_bytes: 60e9, active, speed_projection: projection, context,
  loop_recovery: 'recover', sampling: sampling(temperature),
});
const firstUpstream = 'fedcba9876543210fedcba9876543210fedcba98';
const strata = {
  installed: true,
  version: '477ac13',
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
    model('coder-IQ1_M', 'coder', 'IQ1_M', false, 65536, true, field('temperature', 0.6, 'chat_settings')),
    model('swift-IQ2_XS', 'swift', 'IQ2_XS', false, 32768, false),
  ],
  families: [
    { id: 'qwen', title: 'Qwen3.8-Flash-Next', speed_projection: true },
    { id: 'coder', title: 'Coder', speed_projection: true },
    { id: 'swift', title: 'Swift', speed_projection: false },
  ],
  catalog: [],
  recommended: 'IQ3_XXS',
  contexts: [32768, 65536, 131072],
  default_context: 65536,
  ram_gb: 64,
  disk_free_gb: 412,
  data_dir: 'D:\\Strata-data',
  benchmarks: [
    { model: 'IQ3_XXS', result: '38.5 Token/s', at, decode_tps: 38.5, prompt_tps: 612, context: 65536, speed_projection: false, version: '477ac13' },
    { model: 'coder-IQ1_M', result: '52.3 Token/s', at, decode_tps: 52.3, prompt_tps: 845.5, context: 65536, speed_projection: true, version: '477ac13' },
    { model: 'swift-IQ2_XS', result: '', at, context: 32768, speed_projection: false, version: '477ac13', error: 'BENCH.bat endete mit Code 1' },
  ],
  job: null,
};
const idleMetrics = 'vllm:num_requests_running{model_name="Qwen3-30B-A3B"} 0\nvllm:num_requests_waiting{model_name="Qwen3-30B-A3B"} 0\n';
let summaryMode = 'answer';
let held = [];
const summary = '- **Run setup again** after this update.\n- Long prompts are read faster.\n- A crash when an answer hits max_tokens in a tool call is fixed.';

http.createServer((req, res) => {
  const json = body => {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(body));
  };
  if (req.url.startsWith('/status')) json(status);
  else if (req.url.startsWith('/health')) json({ status: 'ok', loaded: true, service: 'strata', model: 'Qwen3-30B-A3B', max_context: 32768, opencode: '1.19.3' });
  else if (req.url === '/strata') json(strata);
  else if (req.url === '/strata/job') json(null);
  else if (req.url.startsWith('/metrics')) res.end(idleMetrics);
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
