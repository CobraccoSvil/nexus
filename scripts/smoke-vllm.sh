#!/usr/bin/env bash
cd /home/administrator/ideai
python3 -m py_compile brain/providers/vllm_provider.py && echo "syntax OK"
python3 -c "
from brain.providers.vllm_provider import VllmProvider
p = VllmProvider()
print('name:', p.name)
print('endpoint:', p._base_url)
print('catalog:', [m.id for m in p.list_models()])
"
