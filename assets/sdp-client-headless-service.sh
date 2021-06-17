#!/usr/bin/env sh

cat > /etc/appgate.conf <<EOF
[Settings]
LogLevel = $CLIENT_LOGLEVEL
[Credentials]
ControllerUrl = $CLIENT_CONTROLLER_URL
Username = $CLIENT_USERNAME
Password = $CLIENT_PASSWORD
EOF
echo Running service
exec /opt/appgate/service/appgateservice -t /var/run/appgate/.driver.sock --service
