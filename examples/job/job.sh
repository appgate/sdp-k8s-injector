#!/bin/bash

echo "Sleeping for 10 seconds"
sleep 10

echo "Terminating sidecars"
curl -X POST localhost:8000/quitquitquit
curl -X POST localhost:8001/quitquitquit
curl -X POST localhost:8002/quitquitquit

exit 0
