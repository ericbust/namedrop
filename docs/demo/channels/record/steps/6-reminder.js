async ({ page, mark, record }) => {
  await page.getByRole('navigation', { name: 'Direct messages', exact: true }).getByRole('button').filter({ hasText: '@claude' }).click();
  const box = page.getByRole('combobox', { name: 'Message @claude privately', exact: true });
  await box.waitFor();
  await page.waitForTimeout(1500);
  await mark('dm-open', box);
  await box.click();
  await box.pressSequentially('Set a 1-minute reminder here with the text: Tag the release.', { delay: 35 });
  await mark('reminder-typed', box);
  await page.waitForTimeout(1800);
  await box.press('Enter');
  await mark('reminder-sent');
  const reminders = page.locator('.conversation-reminders');
  await reminders.waitFor({ timeout: 120000 });
  await reminders.locator('summary').click();
  const pending = page.getByRole('list', { name: 'Pending reminders', exact: true });
  await pending.waitFor();
  const cancel = pending.getByRole('button', { name: /Cancel/ }).first();
  await cancel.waitFor();
  await page.waitForTimeout(600);
  await mark('reminder-pending', reminders, [pending.locator('.reminder-body').first(), cancel]);
  await page.waitForTimeout(4000);
  await mark('reminder-pending-end', reminders, [pending.locator('.reminder-body').first(), cancel]);
  record.rate(3);
  return await page.locator('main').innerText();
}
