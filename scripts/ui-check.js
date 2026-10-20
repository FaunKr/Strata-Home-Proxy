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
  '/admin/pc/history': ['Awake and asleep', 'Last 7 days', 'Last 12 months', 'From', 'Energy', 'Saved', "today's price"],
  // Benchmarks from scripts/fake-pc.js, the code benchmark from
  // scripts/ui-check-code-bench.json.
  '/admin/strata/version': ['Update available', 'Summary by Strata (Qwen3-30B-A3B)', 'Run setup again', 'Show 3 changes', 'Compare on GitHub', 'Fastest', 'Decode 52.3 tok/s', 'Compare models', 'Total: 19/24 tests', 'Run code benchmark'],
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

// Visits one area and prints its line; returns the number of findings.
async function check(page, errors, name, area, texts, shot) {
  errors.length = 0;
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
  const step = async (name, shot, run) => {
    errors.length = 0;
    let issue = null;
    try {
      await run();
    } catch (e) {
      issue = e.message.split('\n')[0];
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

(async () => {
  const browser = await chromium.launch();
  let findings = 0;
  for (const [name, viewport, colorScheme] of views) {
    const ctx = await browser.newContext({ viewport, colorScheme });
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
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
    }
    await ctx.close();
  }
  await browser.close();
  process.exit(findings ? 1 : 0);
})();
