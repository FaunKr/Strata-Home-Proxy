// Run through scripts/ui-check.sh. Prints one line per area and viewport and
// exits with 1 when any area has a finding.
const fs = require('fs');
const { chromium } = require('playwright');

const base = process.env.BASE;
const areas = ['/admin', '/admin/pc', '/admin/pc/history', '/admin/strata', '/admin/strata/output',
  '/admin/strata/version', '/admin/strata/connection', '/admin/strata/requests', '/admin/access', '/admin/system'];
// Texts an area must show, as innerText gives them: with CSS text-transform
// applied, so e.g. an h3 reads in capitals. The overview's Power row comes from
// scripts/fake-shelly.js, its Hardware card from scripts/fake-pc.js.
// Each text must show at both widths: a column with the class `wide` is
// hidden on a phone, so a text only it shows belongs in a check of its own.
const expected = {
  '/admin': ['87 W', 'Awake', '11.8 / 16.0 GB', '0 rpm', '41 % of limit', 'Last 7 days', 'kWh'],
  // The catalog from scripts/fake-pc.js: Gyro's memory is RAM + VRAM.
  '/admin/strata': ['needs 62 GB RAM (recommended)', 'needs 28 GB RAM + VRAM', 'needs 35 GB RAM + VRAM (not enough RAM + VRAM)', 'DeepSeek-V4.1-Flash UD-Q8_K_XL · 508.4 GB download · needs 306 GB RAM + VRAM (experimental) (does not fit)', '3.5-bit i-quant, the best quality (matches the full model), the slowest. On this PC: fits.'],
  // The program and changelog from scripts/ui-check.sh.
  '/admin/pc': ['Needs a step on the PC', 'Show older changes', 'Reports the PC\'s fan speed', 'Update to 0.3.1'],
  '/admin/pc/history': ['Awake and asleep', 'Last 7 days', 'Last 12 months', 'From', 'Energy', 'Saved', "today's price"],
  // Benchmarks from scripts/fake-pc.js, the code benchmark from
  // scripts/ui-check-code-bench.json.
  // The requests sendRequests made through the proxy.
  '/admin/strata/requests': ['Request log', 'Chat app', 'prompt → tool call', 'Gen. tok/s', '40.0', 'embedding', 'Strata error 404',
    'Speeds by model', 'All keys', 'Qwen3-30B-A3B', '1600.0', 'Generation tok/s over time', 'Generation tok/s by prompt tokens'],
  '/admin/strata/connection': ['Relay through the PC service', 'Off: the proxy reaches Strata directly over the network.', 'Hide model name', 'Public model name'],
  '/admin/strata/version': ['Update available', 'Summary by Strata (Qwen3-30B-A3B)', 'Run setup again', 'Show 3 changes', 'Compare on GitHub', 'Fastest', '52.3 tok/s', 'Not installed', 'BENCH.bat, not comparable', 'Compare models', '24/33', 'Time limit', 'Last run failed', 'Run code benchmark', 'Benchmark all models', 'No result: model did not load', 'Remove old Strata folder'],
};
// After the areas, scripts/fake-pc.js moves upstream on and holds or refuses
// the summary, so the Version tab shows the other two summary states.
const summaryStates = [
  ['writing', 'hold', ['Strata is writing a summary', 'Show 3 changes']],
  ['unavailable', 'fail', ['Summary not available: Strata answers 500', 'Generate summary now', 'Show 3 changes']],
];
const views = [['desk', { width: 1280, height: 900 }, 'light'], ['phone', { width: 390, height: 844 }, 'dark']];
// The same markers as crates/nas-proxy/tests/ui_language.rs.
const german = /[äöüßÄÖÜ]|\b(nicht|und|oder|der|das|ist|wird|bitte|keine?|Abgelehnt|Einstellung|Fehler|Passwort)\b/; // language-check: allow
const password = 'a password of my own';

// A fresh DATA_DIR starts with "admin" and asks for a new password first.
async function login(page) {
  await page.goto(base + '/admin');
  await page.fill('#password', 'admin');
  await page.click('button[type=submit]');
  await page.waitForSelector('#repeat');
  await page.fill('#current', 'admin');
  await page.fill('#new', password);
  await page.fill('#repeat', password);
  // The form sits on /admin itself, so the URL says nothing; the new session
  // cookie comes with the answer to the password change.
  await Promise.all([
    page.waitForResponse(r => r.url().endsWith('/api/change-password') && r.ok()),
    page.click('button[type=submit]'),
  ]);
  await page.waitForSelector('#repeat', { state: 'detached' });
}

// A new build of the version the PC runs is no update; the fake PC still
// reports a build suffix, as older services do, and the card hides it. The
// card hides the older versions behind the toggle otherwise.
async function sameVersion(page, errors, name) {
  await page.goto(base + '/admin/pc', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1500);
  const hidden = !(await page.getByText('Fixes gaming mode staying on').isVisible());
  const program = fs.readFileSync(process.env.PROGRAM);
  fs.writeFileSync(process.env.PROGRAM, 'MZ strata-pc-agent-version:0.1.0\n');
  try {
    const findings = await check(page, errors, name, '/admin/pc (same version)', ['Up to date'], '_admin_pc-same-version');
    if (!hidden) console.log(`FAIL ${name} /admin/pc: 0.2.1 shows outside "Show older changes"`);
    const suffix = (await page.locator('main').innerText()).includes('+ui-check');
    if (suffix) console.log(`FAIL ${name} /admin/pc: shows the build suffix of the installed version`);
    return findings + (hidden ? 0 : 1) + (suffix ? 1 : 0);
  } finally {
    fs.writeFileSync(process.env.PROGRAM, program);
  }
}

// Creates an API key under Access and sends two streamed chat requests with
// it through the proxy, which scripts/fake-pc.js answers with a tool call,
// so the request log has rows. Returns the number of findings.
async function sendRequests(page, errors) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/access', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    await page.fill('#key-name', 'Chat app');
    await page.getByRole('button', { name: 'Create key' }).click(wait);
    const key = await page.locator('#new-key').inputValue(wait);
    // Strata has no embeddings: a row with Strata's error, below the others.
    const embedding = await fetch(base + '/v1/embeddings', {
      method: 'POST',
      headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'any', input: 'hi' }),
    });
    if (embedding.status !== 404) throw new Error(`the embeddings request got ${embedding.status}`);
    for (let i = 0; i < 2; i++) {
      const resp = await fetch(base + '/v1/chat/completions', {
        method: 'POST',
        headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
        body: JSON.stringify({ model: 'any', stream: true, messages: [{ role: 'user', content: 'hi' }] }),
      });
      if (!(await resp.text()).includes('call_1')) throw new Error(`request ${i + 1} got no tool call`);
    }
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} desk requests for the request log${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// On a phone a request's row shows the rest on tap. Returns the number of
// findings.
async function requestDetails(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata/requests', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const first = page.locator('tbody.request').first();
    const prefill = first.locator('tr.request-more').getByText('1600.0 tok/s');
    if (await prefill.isVisible()) throw new Error('the prefill speed shows before the tap');
    await first.locator('tr').first().click(wait);
    await prefill.waitFor(wait);
    // The thinking estimate, a column only a wide screen shows.
    await first.locator('tr.request-more').getByText('≈ 32').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_requests-open.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} request details on tap${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// The speed charts have a dot per chat request, for all keys and for the
// key that sent them. Returns the number of findings.
async function requestCharts(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata/requests', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const dots = page.locator('.chart circle');
    const count = async () => {
      const n = await dots.count();
      if (n !== 4) throw new Error(`the charts have ${n} dots, not 2 each`);
    };
    await count();
    const title = await dots.first().locator('title').textContent();
    if (!title.startsWith('Qwen3-30B-A3B: 40.0 tok/s')) throw new Error(`a dot says "${title}"`);
    await page.selectOption('#request-key', 'Chat app', wait);
    await count();
    await page.locator('.averages').getByText('2', { exact: true }).waitFor(wait);
    await page.selectOption('#request-key', '', wait);
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} request charts and key filter${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Console errors of the current area, listed when the area fails.
const consoleErrors = [];

// Visits one area and prints its line; returns the number of findings.
async function check(page, errors, name, area, texts, shot) {
  errors.length = 0;
  consoleErrors.length = 0;
  await page.goto(base + area.split(' ')[0], { waitUntil: 'domcontentloaded' });
  // The overview stream stays open, so the network is never idle; this
  // gives hydration and the first stream update time.
  await page.waitForTimeout(1500);
  const r = await page.evaluate(() => ({
    overflow: document.documentElement.scrollWidth - window.innerWidth,
    unlabeled: [...document.querySelectorAll('input:not([type=hidden]), select, textarea')]
      .filter(e => !e.labels?.length && !e.getAttribute('aria-label') && !e.getAttribute('aria-labelledby'))
      .map(e => e.id || e.name || e.type),
    small: [...document.querySelectorAll('main button, main a, nav a')]
      .filter(e => { const b = e.getBoundingClientRect(); return b.width > 0 && b.height < 24; })
      .map(e => e.textContent.trim().slice(0, 20)),
    // A wide screen shows the request tables whole; inside their scroll box
    // a cut-off column doesn't make the page scroll.
    cut: window.innerWidth >= 1200 ? [...document.querySelectorAll('.request-table, .averages')]
      .filter(e => e.scrollWidth > e.clientWidth).map(e => `${e.className} by ${e.scrollWidth - e.clientWidth}px`) : [],
    signedOut: !!document.querySelector('#password'),
    text: document.body.innerText,
  }));
  const de = r.text.split('\n').filter(l => german.test(l));
  const issues = [];
  for (const text of texts) {
    if (!r.text.includes(text)) issues.push(`missing "${text}"`);
  }
  if (r.signedOut) issues.push('shows the sign-in form, so nothing behind it was checked');
  if (r.overflow > 0) issues.push(`scrolls sideways by ${r.overflow}px`);
  if (r.cut.length) issues.push(`table scrolls sideways: ${r.cut.join(', ')}`);
  if (r.unlabeled.length) issues.push(`no label: ${r.unlabeled.join(', ')}`);
  if (r.small.length) issues.push(`under 24px: ${r.small.join(', ')}`);
  if (de.length) issues.push(`German: ${de.slice(0, 3).join(' / ')}`);
  if (errors.length) issues.push(`JS error: ${errors[0]}`);
  // A wasm panic shows as "unreachable"; its message went to the console.
  if (issues.length) issues.push(...consoleErrors);
  console.log(`${issues.length ? 'FAIL' : 'ok  '} ${name} ${area}${issues.length ? '\n     ' + issues.join('\n     ') : ''}`);
  if (process.env.SHOTS === '1') {
    await page.screenshot({ path: `${process.env.OUT}/${name}${shot}.jpg`, type: 'jpeg', quality: 60, fullPage: true });
  }
  return issues.length;
}

// Drives Force sleep and Force restart Strata against the states
// scripts/fake-pc.js can take; one line per step, like the areas. Returns the
// number of findings.
async function forced(page, errors) {
  const fakePc = state => fetch(`${process.env.FAKE_PC}/fake/pc?state=${state}`, { method: 'POST' });
  // The overview stream polls the PC every 5 s.
  const wait = { timeout: 15000 };
  const restart = page.getByRole('button', { name: 'Force restart Strata' });
  const force = page.getByRole('button', { name: 'Force sleep' });
  const dialog = page.locator('dialog.confirm');
  let findings = 0;
  // What a failed step names besides its timeout: console errors and the
  // server functions it called.
  const logged = [];
  page.on('console', m => { if (m.type() === 'error') logged.push(`console: ${m.text().slice(0, 300)}`); });
  page.on('response', r => { if (r.url().includes('/admin/api/')) logged.push(`${r.status()} ${new URL(r.url()).pathname}`); });
  const step = async (name, shot, run) => {
    errors.length = 0;
    logged.length = 0;
    let issue = null;
    try {
      await run();
    } catch (e) {
      // The first line says what timed out, the last line of the call log
      // on what, the status lines what the page showed instead.
      const lines = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n');
      const shown = await page.locator('[role=status], [role=alert], dialog[open]').allInnerTexts().catch(() => []);
      issue = [lines[0], lines.length > 1 ? lines.at(-1).trim() : null, shown.length ? `page shows: ${shown.join(' / ')}` : null,
        ...errors.map(e => `JS error: ${e}`), ...logged]
        .filter(Boolean).join('\n     ');
    }
    if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
    console.log(`${issue ? 'FAIL' : 'ok  '} desk ${name}${issue ? '\n     ' + issue : ''}`);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/desk_forced-${shot}.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
    if (issue) findings++;
  };
  const sees = async text => page.getByText(text).first().waitFor(wait);

  await step('Force restart Strata: confirm and result', 'restart', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await restart.click(wait);
    await dialog.getByText('killed hard').waitFor(wait);
    await dialog.getByRole('button', { name: 'Restart Strata' }).click();
    await sees('stays stopped: nothing to start');
  });
  await step('Force restart Strata while Strata loads', 'loading', async () => {
    await fakePc('loading');
    await page.locator('.strata-state .badge', { hasText: 'Loading' }).waitFor(wait);
    await restart.waitFor(wait);
  });
  await step('a Strata the NAS cannot reach shows as not reachable', 'unreachable', async () => {
    await fakePc('unreachable');
    await page.locator('.strata-state .badge', { hasText: 'Not reachable' }).waitFor(wait);
    await sees("Check the PC's firewall");
  });
  await step('Force restart Strata while Strata hangs', 'hung', async () => {
    await fakePc('hung');
    await page.locator('.strata-state .badge', { hasText: 'Offline' }).waitFor(wait);
    await restart.waitFor(wait);
  });
  await step('no Force restart Strata while gaming', 'gaming-strata', async () => {
    await fakePc('gaming');
    await page.locator('.strata-state .badge', { hasText: 'Paused' }).waitFor(wait);
    if (await restart.count()) throw new Error('Force restart Strata shows in gaming mode');
  });
  await step('a failed job keeps its card with the error until closed', 'failed-job', async () => {
    await fetch(`${process.env.FAKE_PC}/fake/last-job?state=failed`, { method: 'POST' });
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await sees("DeepSeek's engine is not available for this computer yet. Nothing has been downloaded.");
    await page.waitForTimeout(1500);
    await page.getByRole('button', { name: 'Close' }).click(wait);
    await page.locator('section.failed-job').waitFor({ state: 'detached', ...wait });
    await fetch(`${process.env.FAKE_PC}/fake/last-job?state=done`, { method: 'POST' });
  });
  await step('a PC without Strata sets up Strata alone, then offers the first model', 'first-install', async () => {
    const strataState = state => fetch(`${process.env.FAKE_PC}/fake/strata?state=${state}`, { method: 'POST' });
    await strataState('none');
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await sees('Then you choose the first model');
    if (await page.locator('select[id="Install-size"]').count()) throw new Error('the setup asks for a model');
    await strataState('first');
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await sees('Choose the first model');
    await page.locator('select[id="AddModel-size"]').waitFor(wait);
    await strataState('installed');
  });
  await step('the model form offers picture reading, ticked by default', 'pictures', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    const tick = page.locator('input[id$="-pictures"]').first();
    await tick.waitFor(wait);
    await sees('Lets the model read pictures');
    if (!(await tick.isChecked())) throw new Error('the Pictures tick is off by default');
  });
  await step("a size that reads no pictures offers no tick and shows Strata's words", 'pictures-size', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    await page.locator('#AddModel-size').selectOption('deepseek-v4.1-flash-UD-Q8_K_XL');
    await sees('On this PC: does not fit');
    await page.locator('#AddModel-pictures').waitFor({ state: 'detached', ...wait });
  });
  await step("the models list shows each model's picture state", 'pictures-state', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await sees('Pictures: on (GPU)');
    await sees('Pictures: on (CPU)');
    await sees('Pictures: off');
  });
  await step('the picture reading switch asks, then the model shows its new state', 'pictures-switch', async () => {
    // No Strata job starts in gaming mode, which the steps above left on.
    await fakePc('awake');
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    // The first row: another one's text can name IQ3_XXS too.
    const row = page.locator('li.model', { hasText: 'IQ3_XXS' }).first();
    const switchOff = row.getByRole('button', { name: 'Switch picture reading off' });
    await switchOff.waitFor(wait);
    await switchOff.click();
    await dialog.getByText('Switch picture reading off for IQ3_XXS?').waitFor(wait);
    await dialog.getByText('The picture file stays on disk').waitFor(wait);
    await dialog.getByRole('button', { name: 'Switch off' }).click();
    // The fake PC finishes the job in 1.5 s; wait for it rather than guess.
    // An earlier step's job stays done until this one reaches the fake PC, so
    // only the switch's own job counts.
    for (let i = 0; i < 30; i++) {
      const job = await (await fetch(`${process.env.FAKE_PC}/strata/job`)).json();
      if (job?.kind === 'set_pictures' && job.state === 'done') break;
      await page.waitForTimeout(500);
    }
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await row.getByText('Pictures: off').waitFor(wait);
    await row.getByRole('button', { name: 'Switch picture reading on' }).waitFor(wait);
    await fakePc('gaming');
  });
  await step('Force sleep in the gaming state', 'gaming-overview', async () => {
    await page.goto(base + '/admin', { waitUntil: 'domcontentloaded' });
    await force.click(wait);
    await dialog.getByText('Gaming mode is on. The PC sleeps anyway').waitFor(wait);
    await dialog.getByRole('button', { name: 'Cancel' }).click();
  });
  await step('Force sleep under a refused Sleep now', 'refused', async () => {
    await fakePc('awake');
    await page.getByRole('button', { name: 'Sleep now' }).click(wait);
    await sees('Refused: Someone used the PC 2 min ago');
    await force.click(wait);
    await dialog.getByText('open requests fail').waitFor(wait);
  });
  await step('forced sleep: the PC sleeps', 'asleep', async () => {
    await dialog.getByRole('button', { name: 'Force sleep' }).click();
    await page.locator('.hero-state', { hasText: 'Asleep' }).waitFor(wait);
    await sees('Forced to sleep; the next request wakes the PC.');
    if (await force.count()) throw new Error('Force sleep still shows while asleep');
  });
  await step('no Force restart Strata while asleep', 'asleep-strata', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await page.locator('.strata-state .badge').waitFor(wait);
    if (await restart.count()) throw new Error('Force restart Strata shows while asleep');
  });
  // The phone pass starts from an awake PC again.
  await fakePc('awake');
  await page.goto(base + '/admin', { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Sleep now' }).waitFor({ timeout: 30000 });
  return findings;
}

// Opens the optimization prompt of the first installed model: the dialog
// with the text and Copy. Returns the number of findings.
async function optimizationPrompt(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await page.getByRole('button', { name: 'Optimization prompt' }).first().click(wait);
    const dialog = page.locator('dialog[open]');
    const text = dialog.locator('textarea');
    await text.waitFor(wait);
    const value = await text.inputValue();
    for (const part of ['I run a local LLM with Strata', '## PC', '## Benchmarks']) {
      if (!value.includes(part)) throw new Error(`prompt lacks "${part}"`);
    }
    await dialog.getByRole('button', { name: 'Copy' }).click();
    await dialog.getByText(/Copied|copy it with Ctrl\+C/).first().waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_optimization-prompt.jpg`, type: 'jpeg', quality: 60 });
    }
    await dialog.getByRole('button', { name: 'Close' }).click();
    await page.locator('dialog[open]').waitFor({ state: 'detached', ...wait });
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} optimization prompt${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Opens a model's earlier speed benchmark runs and the settings of one, then
// a code benchmark run's tasks and a model's earlier code benchmark runs,
// and asks to delete a run and a removed model's runs. Returns the number of findings.
async function benchHistory(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata/version', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    await page.getByRole('button', { name: /^▸ IQ3_XXS/ }).click(wait);
    const earlier = page.locator('tr.bench-earlier');
    await earlier.first().waitFor(wait);
    await earlier.first().getByRole('button', { name: 'Settings' }).click(wait);
    await page.getByText('Preset: coding quick (n_gpu_layers 40, threads 12)').waitFor(wait);
    await page.getByText('Engine environment: GGML_CUDA_GRAPHS=1').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_speed-history.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
    // The code table: a model's details show its tasks.
    const code = page.locator('tr.bench-head').filter({ hasText: '24/33' });
    await code.getByRole('button', { name: 'Details' }).click(wait);
    await page.getByText('Broken: 1 earlier test fails now').waitFor(wait);
    // The repo task's score takes the broken test off.
    const repoTask = page.locator('li').filter({ hasText: '6/9 added tests pass, −1 broken' });
    await repoTask.getByText('5/9', { exact: true }).waitFor(wait);
    await page.getByRole('button', { name: /^▸ coder-IQ1_M/ }).last().click(wait);
    await page.getByText('No result: the model did not load').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_code-history.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    if (overflow > 0) throw new Error(`scrolls sideways by ${overflow}px`);
    // Deleting asks first; cancelled, the runs stay for the steps after.
    const dialog = page.locator('dialog[open]');
    await earlier.first().getByRole('button', { name: /^Delete run of IQ3_XXS/ }).click(wait);
    await dialog.getByText('Delete the run of IQ3_XXS from').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_delete-run.jpg`, type: 'jpeg', quality: 60 });
    }
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await dialog.waitFor({ state: 'detached', ...wait });
    await page.locator('tr.bench-head').filter({ hasText: 'gyro-Gyro-S' })
      .getByRole('button', { name: 'Delete all runs' }).click(wait);
    await dialog.getByText('Delete all runs of gyro-Gyro-S?').waitFor(wait);
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await dialog.waitFor({ state: 'detached', ...wait });
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} benchmark history${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Opens and cancels the question before removing the old Strata folder,
// which scripts/fake-pc.js offers. Returns the number of findings.
async function oldFolder(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata/version', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    await page.getByRole('button', { name: 'Remove old Strata folder' }).click(wait);
    const dialog = page.locator('dialog.confirm');
    await dialog.getByText('The Strata data folder with the models stays').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_old-folder.jpg`, type: 'jpeg', quality: 60 });
    }
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await page.locator('dialog.confirm[open]').waitFor({ state: 'detached', ...wait });
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} remove old Strata folder${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Turns Hide model name on under Strata → Connection and saves it, then
// creates an API key that shows the model's real name and switches it to
// Hide in the key list; all of it again as it was at the end.
// Returns the number of findings.
async function publicName(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata/connection', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const section = page.locator('#strata-connection');
    const toggle = section.getByRole('switch', { name: 'Hide model name' });
    await toggle.click(wait);
    if (await toggle.getAttribute('aria-checked') !== 'true') throw new Error('the switch did not turn on');
    await section.getByRole('button', { name: 'Save' }).click(wait);
    await section.getByText('Saved', { exact: true }).waitFor(wait);
    if (process.env.SHOTS === '1') {
      await section.screenshot({ path: `${process.env.OUT}/${name}_hide-model.jpg`, type: 'jpeg', quality: 60 });
    }
    await toggle.click(wait);
    await section.getByRole('button', { name: 'Save' }).click(wait);
    await section.getByText('Saved', { exact: true }).waitFor(wait);

    await page.goto(base + '/admin/access', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const client = `Script ${name}`;
    await page.fill('#key-name', client);
    await page.selectOption('#key-model-name', 'show');
    await page.getByRole('button', { name: 'Create key' }).click(wait);
    await page.getByText(`New key for “${client}”`).waitFor(wait);
    const row = page.getByLabel(`Model name for ${client}`);
    if (await row.inputValue() !== 'show') throw new Error('the new key does not show the real name');
    await row.selectOption('hide');
    await page.waitForTimeout(1500);
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    if (await page.getByLabel(`Model name for ${client}`).inputValue() !== 'hide') {
      throw new Error('the key list did not keep Hide');
    }
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_key-model-name.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} public model name${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Turns relay mode on under Strata → Connection, sees the switch and its
// end as scripts/fake-pc.js reports them, turns it off again and sees it
// disabled with its reason in gaming mode.
// Returns the number of findings.
async function relay(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata/connection', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const toggle = page.getByLabel('Relay through the PC service');
    await toggle.check(wait);
    await page.getByText('Switching: Strata runs with the new mode from its next start').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_relay-switching.jpg`, type: 'jpeg', quality: 60 });
    }
    await page.getByText('On: Strata listens on the PC only').waitFor(wait);
    await toggle.uncheck(wait);
    await page.getByText('Off: the proxy reaches Strata directly').waitFor(wait);
    // In gaming mode the switch is disabled and says why.
    await fetch(`${process.env.FAKE_PC}/fake/pc?state=gaming`, { method: 'POST' });
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.getByText('Gaming mode is on: switch it off to switch relay mode.').waitFor(wait);
    if (await toggle.isEnabled()) throw new Error('the relay switch can be used in gaming mode');
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_relay-gaming.jpg`, type: 'jpeg', quality: 60 });
    }
    await fetch(`${process.env.FAKE_PC}/fake/pc?state=awake`, { method: 'POST' });
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} relay mode${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Opens the active model's Engine settings: both fields as the PC service
// reported them, Save and restart once changed; a refused save keeps the
// dialog open; Cancel drops the edits.
// Returns the number of findings.
async function engineOptions(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await page.getByRole('button', { name: 'Engine settings' }).first().click(wait);
    const dialog = page.locator('dialog[open]');
    const field = dialog.locator('#IQ3_XXS-engine-options');
    const env = dialog.locator('#IQ3_XXS-engine-env');
    await field.waitFor(wait);
    if ((await field.inputValue()) !== '--threads 8') throw new Error(`options hold "${await field.inputValue()}"`);
    if ((await env.inputValue()) !== 'STRATA_PIN_BY_NEED=1') throw new Error(`env holds "${await env.inputValue()}"`);
    await dialog.getByText('Strata starts with: --native').waitFor(wait);
    const save = dialog.getByRole('button', { name: 'Save and restart' });
    if (!(await save.isDisabled())) throw new Error('Save is enabled before a change');
    await env.fill('STRATA_PIN_BY_NEED=1\nSTRATA_X=1');
    if (await save.isDisabled()) throw new Error('Save stays disabled after a change');
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_engine-settings.jpg`, type: 'jpeg', quality: 60 });
    }
    // A refused save keeps the dialog open with the reason and what was typed.
    await env.fill('9X=1');
    await save.click(wait);
    // The proxy's own refusal, or on the phone pass the PC's (a code
    // benchmark from the desk pass still runs).
    await dialog.getByRole('alert').waitFor(wait);
    if ((await env.inputValue()) !== '9X=1') throw new Error('the refused text is gone');
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await page.locator('dialog[open]').waitFor({ state: 'detached', ...wait });
    await page.getByRole('button', { name: 'Engine settings' }).first().click(wait);
    const again = page.locator('dialog[open] #IQ3_XXS-engine-env');
    if ((await again.inputValue()) !== 'STRATA_PIN_BY_NEED=1') throw new Error('Cancel kept the edit');
    await page.locator('dialog[open]').getByRole('button', { name: 'Cancel' }).click();
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} engine settings${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Shows the 1k | 1M switch for the energy lines at the top of the models
// list: 1k pressed at first; after pressing 1M and reloading the page, 1M is
// pressed and 1k is not, remembered in this browser. Then back to 1k, so a
// later check of this page and a second run of this one see 1k again.
// Returns the number of findings.
async function energyUnit(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const group = page.getByRole('group', { name: 'Energy per' });
    const k = group.getByRole('button', { name: '1k', exact: true });
    const m = group.getByRole('button', { name: '1M', exact: true });
    await k.waitFor(wait);
    const pressed = async button => (await button.getAttribute('aria-pressed')) === 'true';
    if (!(await pressed(k))) throw new Error('1k is not pressed at first');
    if (await pressed(m)) throw new Error('1M is pressed at first');
    await m.click(wait);
    if (await pressed(k)) throw new Error('1k is still pressed after the click');
    if (!(await pressed(m))) throw new Error('1M is not pressed after the click');
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_energy-unit.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(1500);
    const again = page.getByRole('group', { name: 'Energy per' });
    if (!(await pressed(again.getByRole('button', { name: '1M', exact: true })))) {
      throw new Error('1M is not pressed after the reload');
    }
    if (await pressed(again.getByRole('button', { name: '1k', exact: true }))) {
      throw new Error('1k is pressed after the reload');
    }
    // Back to 1k: the stored choice is what every later check of this page runs in.
    await again.getByRole('button', { name: '1k', exact: true }).click(wait);
    if (!(await pressed(again.getByRole('button', { name: '1k', exact: true })))) {
      throw new Error('1k is not pressed after the second click');
    }
    if (await pressed(again.getByRole('button', { name: '1M', exact: true }))) {
      throw new Error('1M is still pressed after the second click');
    }
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n')[0];
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} energy unit switch${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Opens Tune of the first installed model: the dialog with how long and for
// what, closed again without starting; then the preset lines, Undo's and Turn off's questions, and a
// running tune's log and measurements. Returns the number of findings.
async function tune(page, errors, name) {
  const wait = { timeout: 15000 };
  errors.length = 0;
  let issue = null;
  try {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await page.getByText('Last tune (quick, coding)').first().waitFor(wait);
    await page.getByRole('button', { name: 'Tune', exact: true }).first().click(wait);
    const dialog = page.locator('dialog[open]');
    await dialog.getByLabel('How long').selectOption('thorough');
    await dialog.getByLabel('For').selectOption('chat');
    await dialog.getByRole('button', { name: 'Start tuning' }).waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_tune.jpg`, type: 'jpeg', quality: 60 });
    }
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await page.locator('dialog[open]').waitFor({ state: 'detached', ...wait });
    // The preset lines, and Undo asks first.
    await page.getByText('active; re-check offered, because the graphics driver changed').waitFor(wait);
    await page.getByText(': not applied', { exact: false }).first().waitFor(wait);
    await page.getByText('No tuning for this context').waitFor(wait);
    await page.getByRole('button', { name: 'Undo preset' }).first().click(wait);
    await page.locator('dialog[open]').getByText('Undo the preset for IQ3_XXS?').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_preset-undo.jpg`, type: 'jpeg', quality: 60 });
    }
    await page.locator('dialog[open]').getByRole('button', { name: 'Cancel' }).click();
    await page.locator('dialog[open]').waitFor({ state: 'detached', ...wait });
    await page.getByRole('button', { name: 'Apply preset' }).first().waitFor(wait);
    // Turn off asks first too.
    await page.getByRole('button', { name: 'Turn off preset' }).first().click(wait);
    await page.locator('dialog[open]').getByText('Turn the preset off for IQ3_XXS?').waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_preset-off.jpg`, type: 'jpeg', quality: 60 });
    }
    await page.locator('dialog[open]').getByRole('button', { name: 'Cancel' }).click();
    await page.locator('dialog[open]').waitFor({ state: 'detached', ...wait });
    // A tune with nothing measured yet: its running time, no 0 % bar.
    await fetch(`${process.env.FAKE_PC}/fake/tune-start`, { method: 'POST' });
    await page.getByText('Running for 12m').waitFor(wait);
    const bars = await page.locator('section.job progress').count();
    if (bars) throw new Error(`a tune with nothing measured shows ${bars} progress bar(s)`);
    if (process.env.SHOTS === '1') {
      await page.getByText('Running for 12m').scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${process.env.OUT}/${name}_tune-start.jpg`, type: 'jpeg', quality: 60 });
    }
    // A running tune: its step log and its measurements so far.
    await fetch(`${process.env.FAKE_PC}/fake/tune`, { method: 'POST' });
    await page.getByText('[5m] Trying: Keep more of the conversation', { exact: false }).waitFor(wait);
    await page.getByText('Measured so far').waitFor(wait);
    await page.getByText('Your current settings: 171.4 tok/s').waitFor(wait);
    await page.getByText('172.2 tok/s (+0.5 %) · fastest so far', { exact: false }).waitFor(wait);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/${name}_tune-running.jpg`, type: 'jpeg', quality: 60 });
    }
    await page.getByText('Measured so far').waitFor({ state: 'detached', ...wait });
  } catch (e) {
    issue = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n').slice(0, 3).join(' ');
  }
  if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
  console.log(`${issue ? 'FAIL' : 'ok  '} ${name} tune${issue ? '\n     ' + issue : ''}`);
  return issue ? 1 : 0;
}

// Drives "Benchmark all models" against scripts/fake-pc.js: the dialog, then a
// run over all models with its progress. Returns the number of findings.
async function codeBench(page, errors) {
  const wait = { timeout: 15000 };
  let findings = 0;
  const sees = async text => page.getByText(text).first().waitFor(wait);
  const step = async (name, shot, run) => {
    errors.length = 0;
    let issue = null;
    try {
      await run();
    } catch (e) {
      const lines = e.message.replace(/\x1b\[\d+m/g, '').trim().split('\n');
      const shown = await page.locator('[role=status], [role=alert], dialog[open]').allInnerTexts().catch(() => []);
      issue = [lines[0], lines.length > 1 ? lines.at(-1).trim() : null,
        shown.length ? `page shows: ${shown.join(' / ')}` : null, ...errors.map(e => `JS error: ${e}`)]
        .filter(Boolean).join('\n     ');
    }
    if (!issue && errors.length) issue = `JS error: ${errors[0]}`;
    console.log(`${issue ? 'FAIL' : 'ok  '} desk code-benchmark ${name}${issue ? '\n     ' + issue : ''}`);
    if (process.env.SHOTS === '1') {
      await page.screenshot({ path: `${process.env.OUT}/desk_code-benchmark-${shot}.jpg`, type: 'jpeg', quality: 60, fullPage: true });
    }
    if (issue) return 1;
    return 0;
  };

  findings += await step('the dialog and a run over all models', 'all', async () => {
    await page.goto(base + '/admin/strata/version', { waitUntil: 'domcontentloaded' });
    // The model count comes from the Strata answer the area visit above left;
    // give the card time to read it before opening the dialog.
    await page.waitForTimeout(2000);
    await page.getByRole('button', { name: 'Benchmark all models' }).click(wait);
    const dialog = page.locator('dialog.confirm');
    await dialog.getByText('3 models').waitFor(wait);
    await dialog.getByText('4 tasks each').waitFor(wait);
    await dialog.getByText('comes back at the end').waitFor(wait);
    await dialog.getByRole('button', { name: 'Benchmark all models' }).click();
    // The fake PC finishes the first Activate and the runner reports a task
    // running, so the progress reaches the first task of the first model.
    await sees('Model 1 of 3 · task 1 of 4');
  });
  return findings;
}

(async () => {
  const browser = await chromium.launch();
  let findings = 0;
  for (const [name, viewport, colorScheme] of views) {
    const ctx = await browser.newContext({ viewport, colorScheme });
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    page.on('console', m => { if (m.type() === 'error') consoleErrors.push(`console: ${m.text().slice(0, 600)}`); });
    if (name === 'desk') {
      await login(page);
      findings += await sendRequests(page, errors);
    } else {
      await page.goto(base + '/admin');
      if (await page.locator('#password').count()) {
        await page.fill('#password', password);
        await page.click('button[type=submit]');
        await page.waitForSelector('#password', { state: 'detached' });
      }
    }
    for (const area of areas) {
      findings += await check(page, errors, name, area, expected[area] ?? [], area.replace(/\//g, '_'));
    }
    findings += await requestCharts(page, errors, name);
    if (name === 'phone') findings += await requestDetails(page, errors, name);
    findings += await optimizationPrompt(page, errors, name);
    findings += await oldFolder(page, errors, name);
    findings += await benchHistory(page, errors, name);
    findings += await tune(page, errors, name);
    findings += await engineOptions(page, errors, name);
    findings += await energyUnit(page, errors, name);
    findings += await sameVersion(page, errors, name);
    findings += await publicName(page, errors, name);
    if (name === 'desk') {
      for (const [state, mode, texts] of summaryStates) {
        const upstream = state.padEnd(40, '0');
        await fetch(`${process.env.FAKE_PC}/fake/summary?mode=${mode}&upstream=${upstream}`, { method: 'POST' });
        // The first visit starts the summary; a refused one shows on the next.
        if (mode === 'fail') await page.goto(base + '/admin/strata/version', { waitUntil: 'domcontentloaded' });
        findings += await check(page, errors, name, `/admin/strata/version (${state})`, texts, `_admin_strata_version-${state}`);
      }
      // The phone pass starts again from the first upstream and a written summary.
      await fetch(`${process.env.FAKE_PC}/fake/summary?mode=answer`, { method: 'POST' });
      // An unreadable catalog asks for a Strata update instead of the list.
      await fetch(`${process.env.FAKE_PC}/fake/catalog?state=unreadable`, { method: 'POST' });
      findings += await check(page, errors, name, '/admin/strata (catalog unreadable)', ['Update Strata to choose a model', 'IQ3_XXS'], '_admin_strata-unreadable');
      await fetch(`${process.env.FAKE_PC}/fake/catalog?state=read`, { method: 'POST' });
      findings += await relay(page, errors, name);
      findings += await forced(page, errors);
      findings += await codeBench(page, errors);
    }
    await ctx.close();
  }
  await browser.close();
  process.exit(findings ? 1 : 0);
})();
