async ({ page, mark, record }) => {
  const fired = page.locator('article[data-kind="reminder"]').last();
  await fired.waitFor({ timeout: 180000 });
  record.rate(15);
  await mark('reminder-fired', fired);
  const rows = page.locator('article[data-message-id]');
  const before = await rows.count();
  await page.waitForFunction(n => document.querySelectorAll('article[data-message-id]').length > n, before, { timeout: 120000 });
  await page.waitForTimeout(400);
  const answer = rows.last();
  await mark('reminder-answer', [fired, answer], [fired.locator('.body').first(), answer.locator('.body').first()]);
  await page.waitForTimeout(3500);
  return await page.locator('main').innerText();
}
