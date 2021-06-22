# Building docker image

    docker build -f sdp-k8s-client-demo-worker-Dockerfile . -t sdp-k8s-client-demo-worker
    docker tag sdp-k8s-client-demo-worker:latest lhr-nexus01.agi.appgate.com:5001/sdp-k8s-client-demo-worker:latest
    docker push lhr-nexus01.agi.appgate.com:5001/sdp-k8s-client-demo-worker:latest


# Running with docker

    docker run --dns 10.97.2.20  -e DEMO_TIMEOUT=0.1 -e DEMO_UPDATE_INTERVAL=5 -e DEMO_URLS=http://grafana.devops:3000,http://apt.devops sdp-k8s-client-demo-worker

