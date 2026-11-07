async ({ page, mark, record }) => {
  const close = page.getByRole('button', { name: 'Close thread', exact: true });
  if (await close.count()) await close.click();
  const box = page.getByRole('combobox', { name: 'Message #release-planning', exact: true });
  const rows = page.locator('main article[data-message-id]');
  await page.waitForTimeout(1200);
  await mark('collab-open', box);
  await box.click();
  await box.pressSequentially('@claude Reply to me and @codex, and ask @codex for a five-word release risk.', { delay: 35 });
  await box.press('Escape');
  await mark('collab-typed', box);
  await page.waitForTimeout(1800);
  const before = await rows.count();
  await box.press('Enter');
  await page.waitForFunction(n => document.querySelectorAll('main article[data-message-id]').length > n, before);
  const sent = rows.nth(before);
  await mark('collab-sent', sent.locator('.body').first());
  const sentId = await sent.getAttribute('data-message-id');
  await page.waitForTimeout(800);
  await page.getByRole('link', { name: /^All activity/ }).first().click();
  const activity = page.locator('.activity-list').first();
  const items = activity.locator('.activity-item');
  await items.first().waitFor({ timeout: 60000 });
  await page.waitForTimeout(400);
  await mark('activity-open', activity, items.first().locator('.activity-text'));
  let both = false;
  const status = async alias => (await record.helper('status', alias)).trim();
  const seen = new Set();
  let quiet = 0, idleWithoutCodex = 0;
  for (let i = 0; i < 480 && quiet < 10; i++) {
    await page.waitForTimeout(500);
    const now = { claude: await status('claude'), codex: await status('codex') };
    for (const [alias, state] of Object.entries(now)) {
      const name = `collab-${alias}-${state}`;
      if (!seen.has(name) && ['working', 'blocked'].includes(state)) { seen.add(name); await mark(name); }
      if (seen.has(`collab-${alias}-working`) && !seen.has(`collab-${alias}-done`) && ['idle', 'done'].includes(state)) {
        seen.add(`collab-${alias}-done`);
        await mark(`collab-${alias}-done`);
      }
    }
    if (Object.values(now).includes('blocked')) throw new Error('an agent is blocked on a prompt: ' + JSON.stringify(now));
    if (seen.has('collab-claude-done') && !seen.has('collab-codex-working') && ++idleWithoutCodex > 90) throw new Error('Claude finished without bringing in @codex');
    if (!both && (await items.count()) > 1) { both = true; await mark('activity-both', activity, items.nth(1).locator('.activity-text')); }
    quiet = seen.has('collab-codex-done') && seen.has('collab-claude-done') && Object.values(now).every(s => ['idle', 'done'].includes(s)) ? quiet + 1 : 0;
  }
  await mark('collab-done');
  await page.getByRole('navigation', { name: 'Channel list', exact: true }).getByRole('button').filter({ hasText: 'Release planning' }).first().click();
  const posted = page.locator(`main article[data-message-id="${sentId}"]`);
  await posted.waitFor();
  await page.waitForTimeout(1200);
  await mark('collab-channel', posted);
  const reply = posted.locator('.reply-preview');
  if (await reply.count()) {
    await reply.first().click();
    const thread = page.getByRole('complementary', { name: 'Thread', exact: true });
    await thread.getByRole('combobox').waitFor();
    await page.waitForTimeout(1500);
    const messages = thread.locator('article[data-message-id]');
    await mark('collab-thread', [messages.nth(1), messages.nth(2)], [messages.nth(1).locator('.body').first(), messages.nth(2).locator('.body').first()]);
    await page.waitForTimeout(2500);
    return { page: await page.locator('main').innerText(), thread: await thread.innerText() };
  }
  return { page: await page.locator('main').innerText() };
}
