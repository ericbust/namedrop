async ({ page, mark }) => {
  await page.locator('.reply-preview').first().click();
  const thread = page.getByRole('complementary', { name: 'Thread', exact: true });
  const box = thread.getByRole('combobox');
  await box.waitFor();
  const to = thread.locator('.composer-to');
  if (!(await to.innerText()).includes('@codex')) throw new Error('thread composer does not address @codex: ' + await to.innerText());
  await page.waitForTimeout(1200);
  await mark('followup-open', [to, box], to);
  const count = await thread.locator('article[data-message-id]').count();
  await box.click();
  await box.pressSequentially('Two more, please.', { delay: 45 });
  await mark('followup-typed', [to, box], [to, box]);
  await page.waitForTimeout(1800);
  await box.press('Enter');
  await mark('followup-sent');
  await page.waitForFunction(n => document.querySelector('aside[aria-label="Thread"]').querySelectorAll('article[data-message-id]').length >= n, count + 2, { timeout: 120000 });
  await page.waitForTimeout(400);
  const rows = thread.locator('article[data-message-id]');
  const n = await rows.count();
  await mark('followup-reply', [rows.nth(n - 2), rows.nth(n - 1)], rows.nth(n - 1).locator('.body').first());
  await page.waitForTimeout(3500);
  return await thread.innerText();
}
