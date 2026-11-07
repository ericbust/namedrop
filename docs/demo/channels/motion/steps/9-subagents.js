async ({ page, mark, record }) => {
  await page.getByRole('navigation', { name: 'Channel list', exact: true }).getByRole('button').filter({ hasText: 'Release planning' }).first().click();
  const box = page.getByRole('combobox', { name: 'Message #release-planning', exact: true });
  await box.waitFor();
  await page.waitForTimeout(1200);
  await mark('sub-open', box);
  await box.click();
  await box.pressSequentially('@claude Use two subagents to each name one release risk.', { delay: 35 });
  await box.press('Escape');
  await mark('sub-typed', box);
  await page.waitForTimeout(1800);
  await box.press('Enter');
  await mark('sub-sent');
  const count = page.locator('.subagent-count').first();
  await count.waitFor({ timeout: 120000 });
  const chip = count.locator('xpath=ancestor::*[contains(@class,"member")][1]');
  await page.waitForTimeout(400);
  await mark('sub-count', chip, count);
  const seen = await count.innerText();
  for (let i = 0; i < 360; i++) {
    await page.waitForTimeout(500);
    const status = (await record.helper('status', 'claude')).trim();
    if (status === 'blocked') throw new Error('Claude is blocked on a prompt');
    if (['idle', 'done'].includes(status) && !(await page.locator('.subagent-count').count())) break;
  }
  await page.waitForTimeout(1500);
  await mark('sub-done', page.locator('main article[data-message-id]').last());
  await page.waitForTimeout(2500);
  return { seen, page: await page.locator('main').innerText() };
}
