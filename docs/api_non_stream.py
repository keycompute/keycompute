#!/usr/bin/env python3
"""
简单的对话 Demo，使用 OpenAI 兼容格式的 API
"""

import os
from openai import OpenAI

# 配置

# API_URL = "https://api.deepseek.com/v1"
# API_KEY = "sk-70f08cda30ee4e56bd0d27223dec522f"
# API_MODEL = "deepseek-chat"

API_URL = "http://192.168.100.100:3000/v1"
API_KEY = "sk-5f65b519de874eb9a9a2a97f1a12d1c972e6bed188664d24"
API_MODEL = "deepseek-chat"

client = OpenAI(base_url=API_URL, api_key=API_KEY)


def chat():
    messages = []
    print("对话已开始，输入 'quit' 退出\n")

    while True:
        user_input = input("你: ").strip()
        if user_input.lower() == "quit":
            break
        if not user_input:
            continue

        messages.append({"role": "user", "content": user_input})

        try:
            # 非流式响应 (stream=False)
            response = client.chat.completions.create(
                model=API_MODEL,
                messages=messages,
                stream=False,
            )

            print("助手: ", end="", flush=True)
            # 非流式响应直接访问 message.content
            reply = response.choices[0].message.content or ""
            print(reply)
            print()

            messages.append({"role": "assistant", "content": reply})
        except Exception as e:
            print(f"\n错误: {e}\n")
            # 移除失败的消息，允许重试
            messages.pop()


if __name__ == "__main__":
    chat()
