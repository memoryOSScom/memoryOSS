---
description: Store information in long-term memory
argument-hint: [fact, preference, or context to remember]
---

# Remember

Store the given information in memoryOSS long-term memory:

1. Take the content from the arguments
2. If no arguments provided, ask what the user wants to remember
3. Call the `memoryoss_store` MCP tool with the content
4. Add appropriate tags based on the content type (preference, fact, decision, context)
5. Confirm what was stored and that it will be available in future sessions
