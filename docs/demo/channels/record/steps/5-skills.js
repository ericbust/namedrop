async ({ page, mark }) => {
  const close = page.getByRole('button', { name: 'Close thread', exact: true });
  if (await close.count()) await close.click();
  const box = page.getByRole('combobox', { name: 'Message #release-planning', exact: true });
  await page.waitForTimeout(1000);
  await mark('skills-open', box);
  await box.click();
  await box.pressSequentially('@claude /nam', { delay: 70 });
  const list = page.locator('.completions').first();
  await list.waitFor({ timeout: 20000 });
  await page.waitForTimeout(900);
  const items = list.locator('[role=option]');
  await mark('skills-list', list, items.first());
  const text = await list.innerText();
  const count = await items.count();
  await page.waitForTimeout(3800);
  await box.press('Escape');
  await box.fill('');
  await mark('skills-closed');
  return { text, count };
}
