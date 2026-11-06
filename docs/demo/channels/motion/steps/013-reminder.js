async ({page,context,mark,out,record,profile}) => {
const box=page.getByRole('combobox',{name:'Message @claude privately',exact:true});const b=await box.boundingBox(),last=await page.locator('article[data-message-id]').last().boundingBox();
const rect={x:b.x,y:last?last.y:b.y-130,width:b.width,height:b.y+b.height-(last?last.y:b.y-130)};
await mark('reminder-type',rect);await box.pressSequentially('In 2 minutes, reply here with: Review the release checklist. Confirm briefly.',{delay:85});await mark('reminder-instruction',rect);await page.waitForTimeout(2000);await box.press('Enter');await mark('reminder-posted');
await page.locator('.conversation-reminders').waitFor({timeout:90000});await page.locator('.conversation-reminders summary').click();const pending=page.getByRole('list',{name:'Pending reminders',exact:true});await pending.waitFor();const pendingText=await pending.innerText();if(pendingText.includes('0 repeats left'))throw new Error('stale reminder UI');await mark('reminder-pending',await pending.boundingBox());
await page.waitForTimeout(3000);await mark('reminder-confirmed',await pending.boundingBox());await page.screenshot({path:out+'/reminder-pending.png'});record.rate(2);return await page.locator('body').innerText();

}
