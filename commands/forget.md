---
description: Delete memories by searching and selecting
argument-hint: [query to find memories to delete]
---

# Forget

Help the user delete specific memories:

1. If arguments provided, search for matching memories using `memoryoss_recall`
2. Show the results and ask the user which ones to delete
3. Call `memoryoss_forget` with the selected memory IDs
4. Confirm deletion
5. If no arguments, ask what the user wants to forget
