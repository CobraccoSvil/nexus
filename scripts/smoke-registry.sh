#!/usr/bin/env bash
cd /home/administrator/ideai
python3 -m py_compile brain/providers/registry.py && echo "syntax OK"
python3 -c "
from brain.providers.registry import ProviderRegistry
r = ProviderRegistry()
print('providers:', sorted(r._providers.keys()))
print('vllm registered:', 'vllm' in r._providers)
print('vllm models:', [m.id for m in r.list_models('vllm')])
"
