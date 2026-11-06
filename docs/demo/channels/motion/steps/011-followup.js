async ({page,context,mark,out,record,profile}) => {
const thread=page.getByRole('complementary',{name:'Thread',exact:true}),box=thread.getByRole('combobox');
const b=await box.boundingBox(),last=await thread.locator('article[data-message-id]').last().boundingBox();
const rect={x:b.x,y:last?last.y:b.y-160,width:b.width,height:b.y+b.height-(last?last.y:b.y-160)};
const count=await thread.locator('article[data-message-id]').count();await mark('followup-type',rect);await box.pressSequentially('Make it five words.',{delay:115});await mark('followup-instruction',rect);await page.waitForTimeout(2200);await box.press('Enter');await mark('followup-posted');
await page.waitForFunction(n=>document.querySelector('aside[aria-label="Thread"]').querySelectorAll('article[data-message-id]').length>=n,count+2,{timeout:90000});
await mark('followup-reply',await thread.locator('article[data-message-id]').last().boundingBox());await page.waitForTimeout(2000);return await thread.innerText();

}
