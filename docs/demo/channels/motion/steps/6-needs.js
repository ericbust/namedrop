async ({ page, mark, record, out }) => {
  for (let i = 0; i < 120 && !['idle', 'done'].includes((await record.helper('status', 'claude')).trim()); i++) await page.waitForTimeout(500);
  const box = page.getByRole('combobox', { name: 'Message @claude privately', exact: true });
  await box.click();
  await box.pressSequentially('Use Bash to run: touch release-notes.txt', { delay: 35 });
  await mark('needs-typed', box);
  await page.waitForTimeout(1500);
  await box.press('Enter');
  await mark('needs-sent');
  const needs = page.getByRole('navigation', { name: 'Needs you', exact: true });
  const dm = page.getByRole('navigation', { name: 'Direct messages', exact: true }).getByRole('button').filter({ hasText: '@claude' });
  await needs.waitFor({ timeout: 120000 });
  const status = (await record.helper('status', 'claude')).trim();
  if (status !== 'blocked') throw new Error('Claude is ' + status + ', not blocked on a real prompt');
  if (!(await dm.innerText()).includes('Needs you')) throw new Error('the DM row does not say Needs you');
  await page.waitForTimeout(500);
  await mark('needs-blocked', [needs, dm], [needs, dm]);
  await page.waitForTimeout(4500);
  await page.screenshot({ path: out + '/needs-you.png' });
  const screen = await record.helper('screen', 'claude');
  await record.helper('cancel', 'claude');
  await mark('needs-esc');
  await needs.waitFor({ state: 'detached', timeout: 60000 });
  await mark('needs-cancelled', dm, dm);
  await page.waitForTimeout(2500);
  record.stop();
  return { screen, page: await page.locator('main').innerText() };
}
