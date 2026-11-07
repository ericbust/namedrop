async ({ page, mark, record }) => {
  const box = page.getByRole('combobox', { name: 'Message #release-planning', exact: true });
  if (await page.locator('article[data-message-id]').count()) throw new Error('the channel must start empty');
  record.start();
  await mark('ask-wide');
  await page.waitForTimeout(1500);
  await box.click();
  await box.pressSequentially('@codex Suggest a name for the 1.5 release. One word.', { delay: 35 });
  await box.press('Escape');
  await mark('ask-typed', box);
  await page.waitForTimeout(1800);
  await box.press('Enter');
  const sent = page.locator('article[data-message-id]').first();
  await sent.waitFor();
  await mark('ask-sent', sent.locator('.body').first());
  const reply = page.locator('.reply-preview').first();
  await reply.waitFor({ timeout: 120000 });
  await page.waitForTimeout(400);
  await mark('ask-reply', [sent, reply], [sent.locator('.body').first(), reply.locator('.rp-body')]);
  await page.waitForTimeout(3500);
  return await page.locator('main').innerText();
}
