// Run through scripts/ui-check.sh. Prints one line per area and viewport and
// exits with 1 when any area has a finding.
const { chromium } = require('playwright');

const base = process.env.BASE;
const areas = ['/admin', '/admin/pc', '/admin/strata', '/admin/strata/output',
  '/admin/strata/version', '/admin/strata/connection', '/admin/access', '/admin/system'];
const views = [['desk', { width: 1280, height: 900 }, 'light'], ['phone', { width: 390, height: 844 }, 'dark']];
// The same markers as crates/nas-proxy/tests/ui_language.rs.
const german = /[äöüßÄÖÜ]|\b(nicht|und|oder|der|das|ist|wird|bitte|keine?|Abgelehnt|Einstellung|Fehler|Passwort)\b/;
const password = 'ein eigenes Passwort';

// A fresh DATA_DIR starts with "admin" and asks for an own password first.
async function login(page) {
  await page.goto(base + '/admin');
  await page.fill('#password', 'admin');
  await page.click('button[type=submit]');
  await page.waitForSelector('#repeat');
  await page.fill('#current', 'admin');
  await page.fill('#new', password);
  await page.fill('#repeat', password);
  await page.click('button[type=submit]');
  await page.waitForURL(u => !u.pathname.includes('password'));
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
        await page.waitForURL(u => !u.pathname.includes('login'));
      }
    }
    for (const area of areas) {
      errors.length = 0;
      await page.goto(base + area);
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
        text: document.body.innerText,
      }));
      const de = r.text.split('\n').filter(l => german.test(l));
      const issues = [];
      if (r.overflow > 0) issues.push(`scrolls sideways by ${r.overflow}px`);
      if (r.unlabeled.length) issues.push(`no label: ${r.unlabeled.join(', ')}`);
      if (r.small.length) issues.push(`under 24px: ${r.small.join(', ')}`);
      if (de.length) issues.push(`German: ${de.slice(0, 3).join(' / ')}`);
      if (errors.length) issues.push(`JS error: ${errors[0]}`);
      findings += issues.length;
      console.log(`${issues.length ? 'FAIL' : 'ok  '} ${name} ${area}${issues.length ? '\n     ' + issues.join('\n     ') : ''}`);
      if (process.env.SHOTS === '1') {
        await page.screenshot({ path: `${process.env.OUT}/${name}${area.replace(/\//g, '_')}.jpg`, type: 'jpeg', quality: 60, fullPage: true });
      }
    }
    await ctx.close();
  }
  await browser.close();
  process.exit(findings ? 1 : 0);
})();
