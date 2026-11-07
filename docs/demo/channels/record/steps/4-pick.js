async ({ page, mark }) => {
  const thread = page.getByRole('complementary', { name: 'Thread', exact: true });
  const box = thread.getByRole('combobox');
  await box.click();
  await box.pressSequentially('Thanks, both.', { delay: 45 });
  const hint = thread.locator('.composer-status p').first();
  await hint.waitFor();
  await page.waitForTimeout(600);
  await mark('pick-hint', [box, hint], hint);
  const text = await hint.innerText();
  if (!text.startsWith('Pick who gets this')) throw new Error('unexpected hint: ' + text);
  await page.waitForTimeout(3500);
  await box.fill('');
  await mark('pick-cleared');
  return { hint: text };
}
