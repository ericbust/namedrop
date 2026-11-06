async ({page,context,mark,out,record,profile}) => {
const notice=page.getByText(/Reminder for @claude:/).last();await notice.waitFor({timeout:180000});record.rate(12);await mark('reminder-fired',await notice.boundingBox());
await page.getByText('Review the release checklist.',{exact:true}).last().waitFor({timeout:90000});await mark('reminder-reply',await page.getByText('Review the release checklist.',{exact:true}).last().boundingBox());await page.waitForTimeout(2800);await page.screenshot({path:out+'/reminder-fired.png'});return await page.locator('body').innerText();

}
