// The PC with Strata and the PC service for scripts/ui-check.sh, so the
// overview shows an awake PC with its Hardwarewerte.
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

http.createServer((req, res) => {
  const json = body => {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(body));
  };
  if (req.url.startsWith('/status')) json(status);
  else if (req.url.startsWith('/health')) json({ status: 'ok', loaded: true, service: 'strata' });
  else {
    res.statusCode = 404;
    res.end();
  }
}).listen(Number(process.argv[2]), '127.0.0.1');
