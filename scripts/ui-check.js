// Run through scripts/ui-check.sh. Prints one line per area and viewport and
// exits with 1 when any area has a finding.
const { chromium } = require('playwright');

const base = process.env.BASE;
const areas = ['/admin', '/admin/pc', '/admin/pc/history', '/admin/strata', '/admin/strata/output',
  '/admin/strata/version', '/admin/strata/connection', '/admin/access', '/admin/system'];
// Texts an area must show: the overview's Power row comes from
// scripts/fake-shelly.js, its Hardware card from scripts/fake-pc.js.
const expected = {
  '/admin': ['87 W', 'Awake', '11.8 / 16.0 GB', '0 rpm', '41 % of limit', 'Last 7 days', 'kWh'],
  // The catalog from scripts/fake-pc.js: Gyro's memory is RAM + VRAM.
  '/admin/strata': ['needs 62 GB RAM (recommended)', 'needs 28 GB RAM + VRAM', 'needs 35 GB RAM + VRAM (not enough RAM + VRAM)'],
  '/admin/pc/history': ['Awake and asleep', 'Last 7 days', 'Last 12 months', 'From', 'Energy', 'Saved', "today's price"],
  // Benchmarks from scripts/fake-pc.js, the code benchmark from
  // scripts/ui-check-code-bench.json.
  '/admin/strata/version': ['Update available', 'Summary by Strata (Qwen3-30B-A3B)', 'Run setup again', 'Show 3 changes', 'Compare on GitHub', 'Fastest', 'Decode 52.3 tok/s', 'BENCH.bat, not comparable', 'Compare models', 'Total: 25/33 tests', 'Total: 3 of 4 tasks', 'A later run failed', 'Broken: 1 earlier test fails now', 'Run code benchmark', 'Benchmark all models', 'swift-IQ2_XS: model did not load', 'Remove old Strata folder'],
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
  await step('the model form offers picture reading, ticked by default', 'pictures', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    const tick = page.locator('input[id$="-pictures"]').first();
    await tick.waitFor(wait);
    await sees('Lets the model read pictures');
    if (!(await tick.isChecked())) throw new Error('the Pictures tick is off by default');
  });
  await step("the models list shows each model's picture state", 'pictures-state', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await sees('Pictures: on (GPU)');
    await sees('Pictures: on (CPU)');
    await sees('Pictures: off');
  });
  await step('the picture reading switch asks, then the model shows its new state', 'pictures-switch', async () => {
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    const row = page.locator('li.model', { hasText: 'IQ3_XXS' });
    const switchOff = row.getByRole('button', { name: 'Switch picture reading off' });
    await switchOff.waitFor(wait);
    await switchOff.click();
    await dialog.getByText('Switch picture reading off for IQ3_XXS?').waitFor(wait);
    await dialog.getByText('Lets the model read pictures').waitFor(wait);
    await dialog.getByRole('button', { name: 'Switch off' }).click();
    // The fake PC finishes the job, then the model reads no pictures.
    await page.waitForTimeout(2500);
    await page.goto(base + '/admin/strata', { waitUntil: 'domcontentloaded' });
    await row.getByText('Pictures: off').waitFor(wait);
    await row.getByRole('button', { name: 'Switch picture reading on' }).waitFor(wait);
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
    if (name === 'desk') await login(page);
    else {
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
    findings += await optimizationPrompt(page, errors, name);
    findings += await oldFolder(page, errors, name);
    findings += await tune(page, errors, name);
    findings += await engineOptions(page, errors, name);
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
      findings += await forced(page, errors);
      findings += await codeBench(page, errors);
    }
    await ctx.close();
  }
  await browser.close();
  process.exit(findings ? 1 : 0);
})();
