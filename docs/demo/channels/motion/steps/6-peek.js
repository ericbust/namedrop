async ({ page, mark, record }) => {
  await page.getByRole('navigation', { name: 'Direct messages', exact: true }).getByRole('button').filter({ hasText: '@codex' }).click();
  await page.waitForTimeout(800);
  const before = await record.helper('screen', 'codex');
  await page.getByRole('button', { name: 'Peek', exact: true }).click();
  await page.getByRole('button', { name: 'Expand' }).click();
  const drawer = page.getByRole('complementary', { name: 'Screen of @codex', exact: true });
  await drawer.waitFor();
  const terminal = drawer.locator('.peek-terminal');
  await page.getByRole('button', { name: 'Type to agent', exact: true }).click();
  const box = drawer.getByLabel('Message to @codex', { exact: true });
  await box.waitFor();
  await page.waitForTimeout(1200);
  await mark('peek-open', drawer.locator('.peek-live'), terminal);
  await box.click();
  await box.pressSequentially('Name one release risk in five words.', { delay: 40 });
  await mark('peek-typed', [drawer.locator('.peek-compose-head'), box], box);
  await page.waitForTimeout(1800);
  await drawer.getByRole('button', { name: 'Send', exact: true }).click();
  await mark('peek-sent');
  let working = false;
  for (let i = 0; i < 20 && !working; i++) {
    await page.waitForTimeout(500);
    working = (await record.helper('status', 'codex')).trim() === 'working';
  }
  if (!working) {
    await drawer.getByRole('button', { name: 'Send Enter', exact: true }).click();
    await mark('peek-extra-enter');
  }
  for (let i = 0; i < 120; i++) {
    await page.waitForTimeout(500);
    const status = (await record.helper('status', 'codex')).trim();
    if (['idle', 'done'].includes(status) && i > 2) break;
  }
  await page.waitForTimeout(800);
  await mark('peek-reply', drawer.locator('.peek-live'), terminal);
  await page.waitForTimeout(3500);
  const screen = await record.helper('screen', 'codex');
  record.stop();
  return { before, screen };
}
